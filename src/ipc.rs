//! Связь панели и всплывающих окон (HTML/JS) с Rust.
//!
//! Панель → Rust: `window.cefQuery({request: JSON})` через message router
//! из `cef::wrapper`. Функция `cefQuery` внедряется только в браузер панели
//! (см. `app.rs`), а здесь дополнительно проверяется, что запрос пришёл
//! именно оттуда: обычные сайты управлять браузером не могут.
//!
//! Rust → панель: `window.rustium.update(state)` через `execute_java_script`
//! (см. `Shell::push_state`).

use crate::downloads::{Badge, Download};
use crate::shell::{Action, Shell, TabId};
use crate::settings::{self, SettingsCommand};
use crate::task::post_ui;
use cef::wrapper::message_router::{BrowserSideCallback, BrowserSideHandler};
use cef::*;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, Weak};

/// Команды, которые панель может прислать.
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Command {
    /// Панель загрузилась и ждёт текущее состояние.
    Ready,
    Navigate {
        url: String,
    },
    Back,
    Forward,
    Reload,
    Stop,
    NewTab,
    CloseTab {
        id: TabId,
    },
    SelectTab {
        id: TabId,
    },
    /// Перенести фокус из адресной строки в страницу (Esc).
    FocusContent,
    DevTools,
    /// Открыть всплывающее окно под кнопкой `anchor` (координаты панели).
    ShowPopup {
        kind: PopupKind,
        anchor: Anchor,
    },
    ClosePopup,
    /// Открыть список загрузок под кнопкой загрузок (пункт меню).
    OpenDownloads,
    /// Всплывающее окно отрисовало содержимое и сообщает свой размер.
    PopupSize {
        width: f64,
        height: f64,
    },
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ToggleFullscreen,
    Print,
    Quit,
    Download {
        id: u32,
        action: DownloadAction,
    },
    /// Где показывать DevTools.
    SetDevtoolsDock {
        dock: Dock,
    },
    /// Разделитель между страницей и DevTools сдвинули на (dx, dy).
    SplitterMove {
        dx: f64,
        dy: f64,
    },
    OpenSettings,
    /// Ответ в окне запроса разрешения.
    PermissionDecision {
        id: u64,
        allow: bool,
        remember: bool,
    },
}

/// Где показывать DevTools: справа, снизу или отдельным окном.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Dock {
    #[default]
    Right,
    Bottom,
    Window,
}

/// Команды служебных страниц `rustium://` во вкладках (страницы ошибок).
/// Набор узкий: такие страницы не управляют браузером, а только своей
/// вкладкой, и браузер сам решает, применимо ли действие.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum PageCommand {
    /// Повторить загрузку исходного адреса.
    Retry,
    /// Ошибка сертификата: открыть сайт, несмотря на неё.
    ProceedUnsafe,
    /// Файл не найден: открыть папку.
    RevealFile,
    /// Нет сети: открыть системные настройки сети.
    NetworkSettings,
}

/// Какое всплывающее окно показать.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PopupKind {
    Menu,
    Downloads,
    /// Запрос разрешения сайта (под значком сайта слева).
    Permission,
}

/// Прямоугольник кнопки, под которой открывается всплывающее окно.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Anchor {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Действия со строкой списка загрузок.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadAction {
    Pause,
    Resume,
    Cancel,
    Open,
    Show,
    Retry,
    Remove,
}

impl Command {
    fn into_action(self) -> Option<Action> {
        Some(match self {
            Command::Ready => return None,
            Command::Navigate { url } => Action::Navigate(url),
            Command::Back => Action::Back,
            Command::Forward => Action::Forward,
            Command::Reload => Action::Reload,
            Command::Stop => Action::Stop,
            Command::NewTab => Action::NewTab,
            Command::CloseTab { id } => Action::CloseTab(id),
            Command::SelectTab { id } => Action::SelectTab(id),
            Command::FocusContent => Action::FocusContent,
            Command::DevTools => Action::DevTools,
            Command::ShowPopup { kind, anchor } => Action::ShowPopup { kind, anchor },
            Command::ClosePopup => Action::ClosePopup,
            Command::OpenDownloads => Action::OpenDownloads,
            Command::PopupSize { width, height } => Action::PopupSize { width, height },
            Command::ZoomIn => Action::ZoomIn,
            Command::ZoomOut => Action::ZoomOut,
            Command::ZoomReset => Action::ZoomReset,
            Command::ToggleFullscreen => Action::ToggleFullscreen,
            Command::Print => Action::Print,
            Command::Quit => Action::Quit,
            Command::Download { id, action } => Action::Download { id, action },
            Command::SetDevtoolsDock { dock } => Action::SetDevtoolsDock(dock),
            Command::SplitterMove { dx, dy } => Action::SplitterMove { dx, dy },
            Command::OpenSettings => Action::OpenSettings,
            Command::PermissionDecision { id, allow, remember } => {
                Action::PermissionDecision { id, allow, remember }
            }
        })
    }
}

/// Значок безопасности в адресной строке.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Security {
    /// Ничего не показываем (новая вкладка, file://, localhost).
    None,
    /// https: замок.
    Secure,
    /// http: «Not secure».
    Insecure,
    /// Ошибка сертификата: красный «Not secure», зачёркнутый https.
    CertError,
    /// Служебная страница браузера: «Rustium».
    Internal,
}

impl Security {
    pub fn for_url(url: &str) -> Self {
        let lower = url.to_ascii_lowercase();
        let host = lower
            .split_once("://")
            .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""))
            .unwrap_or("");
        let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
        if lower.starts_with("https://") {
            Security::Secure
        } else if lower.starts_with("http://") {
            if host == "localhost" || host == "127.0.0.1" || host == "[::1]" {
                Security::None
            } else {
                Security::Insecure
            }
        } else if ["rustium:", "about:", "chrome:", "devtools:", "data:"]
            .iter()
            .any(|p| lower.starts_with(p))
        {
            Security::Internal
        } else {
            Security::None
        }
    }
}

/// Состояние одной вкладки для панели.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabState {
    pub id: TabId,
    pub title: String,
    pub url: String,
    pub favicon: Option<String>,
    pub loading: bool,
    /// 0.0..=1.0
    pub progress: f64,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub security: Security,
    /// Страница пользуется камерой / микрофоном.
    pub camera: bool,
    pub microphone: bool,
    /// Сайт ждёт ответа на запрос разрешения.
    pub permission_pending: bool,
}

/// Всё, что панель отображает.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiState {
    pub tabs: Vec<TabState>,
    pub active: Option<TabId>,
    /// Окно в полноэкранном режиме (на macOS тогда нет кнопок окна).
    pub fullscreen: bool,
    /// Окно в фокусе (неактивное окно панель рисует приглушённо).
    pub window_active: bool,
    /// Масштаб активной вкладки, %.
    pub zoom: u32,
    /// Новые загрузки сверху.
    pub downloads: Vec<Download>,
    pub downloads_badge: Badge,
    /// Общий прогресс активных загрузок, 0.0..=1.0.
    pub downloads_progress: f64,
    /// Открытое всплывающее окно.
    pub popup: Option<PopupKind>,
    pub devtools_dock: Dock,
    /// У активной вкладки открыты DevTools.
    pub devtools_open: bool,
    /// Запрос разрешения активной вкладки.
    pub permission: Option<crate::shell::PromptView>,
}

/// Обработчик запросов `cefQuery` из панели и всплывающих окон.
pub struct ToolbarQueryHandler {
    shell: Weak<Shell>,
}

impl ToolbarQueryHandler {
    pub fn new(shell: Weak<Shell>) -> Arc<Self> {
        Arc::new(Self { shell })
    }
}

impl BrowserSideHandler for ToolbarQueryHandler {
    fn on_query_str(
        &self,
        browser: Option<Browser>,
        frame: Option<Frame>,
        _query_id: i64,
        request: &str,
        _persistent: bool,
        callback: Arc<Mutex<dyn BrowserSideCallback>>,
    ) -> bool {
        let Some(shell) = self.shell.upgrade() else {
            return false;
        };
        let reply = |result: Result<String, (i32, String)>| {
            if let Ok(cb) = callback.lock() {
                match result {
                    Ok(body) => cb.success_str(&body),
                    Err((code, msg)) => cb.failure(code, &msg),
                }
            }
        };

        let main_frame = frame.as_ref().is_some_and(|f| f.is_main() != 0);
        let frame_url = frame
            .as_ref()
            .map(|f| CefString::from(&f.url()).to_string())
            .unwrap_or_default();
        // Служебная страница rustium:// во вкладке: только её команды.
        let service_tab = main_frame && frame_url.starts_with("rustium://");
        let tab = browser.as_ref().and_then(|b| shell.tab_of(b));
        let settings_page = frame_url == "rustium://settings"
            || frame_url.starts_with("rustium://settings/")
            || frame_url.starts_with("rustium://settings?");
        if let (true, true, Some(_)) = (service_tab, settings_page, tab) {
            match serde_json::from_str::<SettingsCommand>(request) {
                Ok(command) => settings_command(&shell, browser, command, reply_owned(callback)),
                Err(err) => reply(Err((400, err.to_string()))),
            }
            return true;
        }
        if let (true, Some(tab)) = (service_tab, tab) {
            match serde_json::from_str::<PageCommand>(request) {
                Ok(command) => {
                    reply(Ok(String::new()));
                    post_ui(move || shell.page_command(tab, command));
                }
                Err(err) => reply(Err((400, err.to_string()))),
            }
            return true;
        }

        // Остальное — только из главного фрейма панели или всплывающего окна.
        let from_ui = main_frame && browser.as_ref().is_some_and(|b| shell.is_ui_browser(b));
        let Some(browser) = browser.filter(|_| from_ui) else {
            reply(Err((403, "forbidden".into())));
            return true;
        };

        match serde_json::from_str::<Command>(request) {
            Ok(Command::Ready) => reply(Ok(shell.ui_ready(&browser))),
            Ok(cmd) => {
                reply(Ok(String::new()));
                if let Some(action) = cmd.into_action() {
                    // Выполняем вне обработчика router'а: он держит свою
                    // блокировку, пока мы внутри on_query_str.
                    post_ui(move || shell.perform(action));
                }
            }
            Err(err) => reply(Err((400, err.to_string()))),
        }
        true
    }
}

type Reply = Box<dyn FnOnce(Result<String, (i32, String)>) + Send>;

fn reply_owned(callback: Arc<Mutex<dyn BrowserSideCallback>>) -> Reply {
    Box::new(move |result| {
        if let Ok(cb) = callback.lock() {
            match result {
                Ok(body) => cb.success_str(&body),
                Err((code, msg)) => cb.failure(code, &msg),
            }
        }
    })
}

/// Команды страницы настроек. CEF зовём только то, что не вызывает наши
/// обработчики синхронно; остальное — через post_ui.
fn settings_command(shell: &Arc<Shell>, browser: Option<Browser>, command: SettingsCommand, reply: Reply) {
    match command {
        SettingsCommand::GetSettings => reply(Ok(settings::settings_json(true))),
        SettingsCommand::SetSettings { prefs } => match crate::prefs::set(*prefs) {
            Ok((new, old)) => {
                reply(Ok(settings::settings_json(false)));
                let shell = shell.clone();
                post_ui(move || {
                    settings::apply_changes(&new, &old);
                    shell.reapply_site_settings();
                });
            }
            Err(message) => reply(Err((400, message))),
        },
        SettingsCommand::ChooseDownloadDir => {
            let Some(host) = browser.and_then(|b| b.host()) else {
                reply(Ok(r#"{"dir":null}"#.into()));
                return;
            };
            let current = crate::prefs::get()
                .downloads
                .dir
                .unwrap_or_else(|| crate::downloads::downloads_dir().to_string_lossy().into_owned());
            let reply = Arc::new(Mutex::new(Some(reply)));
            post_ui(move || {
                let mut callback = FolderChosen::new(reply);
                host.run_file_dialog(
                    FileDialogMode::OPEN_FOLDER,
                    Some(&CefString::from("Choose a download folder")),
                    Some(&CefString::from(current.as_str())),
                    None,
                    Some(&mut callback),
                );
            });
        }
        SettingsCommand::MakeDefaultBrowser => {
            let value = settings::make_default_browser();
            reply(Ok(serde_json::json!({ "defaultBrowser": value }).to_string()));
        }
        SettingsCommand::OpenProxySettings => {
            reply(Ok("null".into()));
            post_ui(settings::open_proxy_settings);
        }
    }
}

wrap_run_file_dialog_callback! {
    struct FolderChosen {
        reply: Arc<Mutex<Option<Reply>>>,
    }

    impl RunFileDialogCallback {
        fn on_file_dialog_dismissed(&self, file_paths: Option<&mut CefStringList>) {
            let dir = file_paths
                .map(std::mem::take)
                .and_then(|list| list.into_iter().next());
            let reply = self.reply.lock().ok().and_then(|mut r| r.take());
            if let Some(reply) = reply {
                reply(Ok(serde_json::json!({ "dir": dir }).to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_toolbar_commands() {
        let c: Command =
            serde_json::from_str(r#"{"cmd":"navigate","url":"rust-lang.org"}"#).unwrap();
        assert!(matches!(c, Command::Navigate { url } if url == "rust-lang.org"));
        let c: Command = serde_json::from_str(r#"{"cmd":"closeTab","id":3}"#).unwrap();
        assert!(matches!(c, Command::CloseTab { id: 3 }));
        let c: Command = serde_json::from_str(r#"{"cmd":"focusContent"}"#).unwrap();
        assert!(matches!(c, Command::FocusContent));
        assert!(serde_json::from_str::<Command>(r#"{"cmd":"rm -rf"}"#).is_err());
        let c: Command = serde_json::from_str(
            r#"{"cmd":"showPopup","kind":"downloads","anchor":{"x":1,"y":2,"width":30,"height":30}}"#,
        )
        .unwrap();
        assert!(matches!(c, Command::ShowPopup { kind: PopupKind::Downloads, .. }));
        let c: Command =
            serde_json::from_str(r#"{"cmd":"download","id":4,"action":"show"}"#).unwrap();
        assert!(matches!(c, Command::Download { id: 4, action: DownloadAction::Show }));
        assert!(serde_json::from_str::<Command>(r#"{"cmd":"download","id":4,"action":"rm"}"#).is_err());
        let c: Command =
            serde_json::from_str(r#"{"cmd":"setDevtoolsDock","dock":"window"}"#).unwrap();
        assert!(matches!(c, Command::SetDevtoolsDock { dock: Dock::Window }));
    }

    #[test]
    fn parses_page_commands() {
        let c: PageCommand = serde_json::from_str(r#"{"cmd":"proceedUnsafe","url":"x"}"#).unwrap();
        assert_eq!(c, PageCommand::ProceedUnsafe);
        assert_eq!(serde_json::from_str::<PageCommand>(r#"{"cmd":"retry"}"#).unwrap(), PageCommand::Retry);
        // Команды панели со служебной страницы не проходят.
        assert!(serde_json::from_str::<PageCommand>(r#"{"cmd":"quit"}"#).is_err());
        assert!(serde_json::from_str::<PageCommand>(r#"{"cmd":"navigate","url":"x"}"#).is_err());
    }

    #[test]
    fn state_is_camel_case() {
        let s = UiState {
            tabs: vec![TabState {
                id: 1,
                title: "t".into(),
                url: "u".into(),
                favicon: None,
                loading: true,
                progress: 0.5,
                can_go_back: false,
                can_go_forward: true,
                security: Security::Secure,
                camera: false,
                microphone: false,
                permission_pending: false,
            }],
            active: Some(1),
            fullscreen: false,
            window_active: true,
            zoom: 100,
            downloads: vec![],
            downloads_badge: Badge::Hidden,
            downloads_progress: 0.0,
            popup: Some(PopupKind::Menu),
            devtools_dock: Dock::Bottom,
            devtools_open: false,
            permission: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"canGoForward\":true"));
        assert!(json.contains("\"active\":1"));
        assert!(json.contains("\"windowActive\":true"));
        assert!(json.contains("\"downloadsBadge\":\"hidden\""));
        assert!(json.contains("\"popup\":\"menu\""));
        assert!(json.contains("\"devtoolsDock\":\"bottom\""));
        assert!(json.contains("\"security\":\"secure\""));
    }

    #[test]
    fn security_badge_for_url() {
        assert_eq!(Security::for_url("https://docs.rs/cef"), Security::Secure);
        assert_eq!(Security::for_url("http://neverssl.com/online"), Security::Insecure);
        assert_eq!(Security::for_url("http://localhost:3000/"), Security::None);
        assert_eq!(Security::for_url("http://127.0.0.1"), Security::None);
        assert_eq!(Security::for_url("rustium://history"), Security::Internal);
        assert_eq!(Security::for_url("file:///etc/hosts"), Security::None);
    }
}
