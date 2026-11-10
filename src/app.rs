//! CefApp, обработчики процессов и точки входа.

use crate::shell::{Shell, UI_MARKER};
use cef::wrapper::message_router::{
    MessageRouterConfig, MessageRouterRendererSide, MessageRouterRendererSideHandlerCallbacks,
    RendererSideRouter,
};
use cef::*;
use std::os::raw::c_int;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

wrap_app! {
    pub struct RustiumApp {
        shell: Arc<Shell>,
        renderer_router: Arc<RendererSideRouter>,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            // Пустой process_type — браузерный (главный) процесс.
            let is_browser = process_type.map(|p| p.to_string().is_empty()).unwrap_or(true);
            let Some(command_line) = command_line else { return };
            if !is_browser {
                return;
            }
            // В отладочной сборке не трогаем связку ключей macOS
            // (иначе система спрашивает пароль при каждом запуске
            // неподписанного приложения).
            if cfg!(all(target_os = "macos", debug_assertions)) {
                command_line.append_switch(Some(&CefString::from("use-mock-keychain")));
            }
            crate::settings::apply_switches(command_line);
        }

        // Схема rustium:// должна быть зарегистрирована во всех процессах.
        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            if let Some(registrar) = registrar {
                crate::scheme::register_custom_scheme(registrar);
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(RustiumBrowserProcessHandler::new(self.shell.clone()))
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(RustiumRenderProcessHandler::new(self.renderer_router.clone()))
        }
    }
}

fn new_app() -> App {
    RustiumApp::new(
        Shell::new(),
        RendererSideRouter::new(MessageRouterConfig::default()),
    )
}

wrap_browser_process_handler! {
    struct RustiumBrowserProcessHandler {
        shell: Arc<Shell>,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            crate::scheme::register_handler_factory();
            self.shell.start();
        }
    }
}

// -------------------------------------------------------------------------
// Renderer: внедряем cefQuery только в браузер панели
// -------------------------------------------------------------------------

/// Идентификаторы браузеров панели в текущем renderer-процессе.
static UI_BROWSERS: Mutex<Vec<c_int>> = Mutex::new(Vec::new());

fn is_ui_browser(browser: &Browser) -> bool {
    let id = browser.identifier();
    UI_BROWSERS
        .lock()
        .map(|ids| ids.contains(&id))
        .unwrap_or(false)
}

wrap_render_process_handler! {
    struct RustiumRenderProcessHandler {
        router: Arc<RendererSideRouter>,
    }

    impl RenderProcessHandler {
        fn on_browser_created(
            &self,
            browser: Option<&mut Browser>,
            extra_info: Option<&mut DictionaryValue>,
        ) {
            let marked = extra_info.is_some_and(|d| d.bool(Some(&CefString::from(UI_MARKER))) != 0);
            if let (true, Some(browser)) = (marked, browser) {
                if let Ok(mut ids) = UI_BROWSERS.lock() {
                    ids.push(browser.identifier());
                }
            }
        }

        fn on_browser_destroyed(&self, browser: Option<&mut Browser>) {
            if let (Some(browser), Ok(mut ids)) = (browser, UI_BROWSERS.lock()) {
                let id = browser.identifier();
                ids.retain(|x| *x != id);
            }
        }

        fn on_context_created(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            let Some(browser) = browser else { return };
            let is_main = frame.as_deref().is_some_and(|f| f.is_main() != 0);
            // cefQuery получают интерфейс браузера и служебные страницы
            // rustium:// (их нельзя открыть со страниц сайтов). Что им
            // можно делать, всё равно проверяет браузерный процесс.
            let service_page = frame
                .as_deref()
                .is_some_and(|f| CefString::from(&f.url()).to_string().starts_with("rustium://"));
            if is_main && (is_ui_browser(browser) || service_page) {
                self.router.on_context_created(Some(browser.clone()), frame.cloned(), context.cloned());
            }
        }

        fn on_context_released(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            let Some(browser) = browser else { return };
            // Освобождаем всегда: router сам знает, где он внедрён.
            {
                self.router.on_context_released(Some(browser.clone()), frame.cloned(), context.cloned());
            }
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            self.router.on_process_message_received(
                browser.cloned(),
                frame.cloned(),
                Some(source_process),
                message.cloned(),
            ) as c_int
        }
    }
}

// -------------------------------------------------------------------------
// Настройки и запуск
// -------------------------------------------------------------------------

/// Папка профиля (cookies, кэш, localStorage).
/// Можно переопределить переменной окружения RUSTIUM_PROFILE.
fn profile_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RUSTIUM_PROFILE") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Rustium")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("rustium")
    }
}

fn settings() -> Settings {
    let root = profile_dir();
    let _ = std::fs::create_dir_all(&root);
    let cache = root.join("Default");

    // Linux: песочнице Chromium нужен SUID chrome-sandbox или user
    // namespaces (на Ubuntu 24.04 их ограничивает AppArmor). Для
    // прототипа по умолчанию выключаем; RUSTIUM_SANDBOX=1 — включить.
    // macOS: песочница включена (её инициализируют helper-процессы).
    let no_sandbox = cfg!(target_os = "linux") && std::env::var_os("RUSTIUM_SANDBOX").is_none();

    // RUSTIUM_DEBUG_PORT=9222 — удалённая отладка: chrome://inspect,
    // Playwright/Puppeteer (connectOverCDP) и т.п.
    let remote_debugging_port = std::env::var("RUSTIUM_DEBUG_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);

    Settings {
        no_sandbox: no_sandbox as c_int,
        root_cache_path: CefString::from(root.to_string_lossy().as_ref()),
        cache_path: CefString::from(cache.to_string_lossy().as_ref()),
        log_severity: LogSeverity::WARNING,
        remote_debugging_port,
        background_color: crate::shell::CHROME_BG,
        ..Default::default()
    }
}

/// Объявляем libcef версию API, под которую сгенерированы привязки.
fn declare_api_version() {
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
}

/// Главный процесс.
pub fn run_main() -> i32 {
    #[cfg(target_os = "macos")]
    let _library = match crate::mac::load_cef_framework(false) {
        Some(library) => library,
        None => return 1,
    };
    declare_api_version();
    #[cfg(target_os = "macos")]
    crate::mac::init_application();

    let args = args::Args::new();
    let mut app = new_app();

    // На Linux/Windows этот же бинарник запускается как subprocess
    // (renderer, GPU, utility). Тогда execute_process вернёт код >= 0.
    #[cfg(not(target_os = "macos"))]
    {
        let code = execute_process(
            Some(args.as_main_args()),
            Some(&mut app),
            std::ptr::null_mut(),
        );
        if code >= 0 {
            return code;
        }
    }

    // Настройки нужны до инициализации: часть из них — ключи Chromium.
    crate::prefs::init(&profile_dir());
    #[cfg(target_os = "macos")]
    crate::mac::install_url_handler();

    if initialize(
        Some(args.as_main_args()),
        Some(&settings()),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        eprintln!(
            "rustium: CEF не инициализировался (код {}).",
            get_exit_code()
        );
        eprintln!("Возможно, уже запущен другой экземпляр с этим профилем.");
        return 1;
    }
    run_message_loop();
    shutdown();
    0
}

/// Helper-процесс (только macOS: «Rustium Helper*.app» внутри бандла).
pub fn run_helper() -> i32 {
    #[cfg(target_os = "macos")]
    {
        let args = args::Args::new();
        let mut sandbox = cef::sandbox::Sandbox::new();
        sandbox.initialize(args.as_main_args());
        let Some(_library) = crate::mac::load_cef_framework(true) else {
            return 1;
        };
        declare_api_version();
        let mut app = new_app();
        execute_process(
            Some(args.as_main_args()),
            Some(&mut app),
            std::ptr::null_mut(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("rustium_helper нужен только на macOS; на этой платформе запускайте `rustium`.");
        1
    }
}
