//! Обработчики событий браузеров (CefClient и его handler'ы).
//!
//! Один и тот же набор типов обслуживает и панель, и вкладки;
//! различаются они полем `role`.

use crate::downloads::{self, Snapshot};
use crate::shell::{Action, Shell, TabId};
use crate::task::post_ui;
use crate::prefs::{PermKind, Setting};
use crate::{settings, ui};
use cef::wrapper::message_router::MessageRouterBrowserSideHandlerCallbacks;
use cef::*;
use std::os::raw::c_int;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Браузер с HTML-панелью (вкладки, адресная строка).
    Toolbar,
    /// Всплывающее окно поверх страницы (меню, загрузки).
    Popup,
    /// Разделитель между страницей и встроенными DevTools.
    Splitter,
    /// DevTools вкладки (встроенные или отдельным окном).
    DevTools(TabId),
    /// Вкладка с обычным сайтом.
    Tab(TabId),
}

impl Role {
    /// Браузер интерфейса (панель или всплывающее окно): ему доступен cefQuery.
    pub fn is_ui(self) -> bool {
        matches!(self, Role::Toolbar | Role::Popup | Role::Splitter)
    }

    fn log_prefix(self) -> &'static str {
        match self {
            Role::Popup => "[popup]",
            Role::Splitter => "[splitter]",
            _ => "[toolbar]",
        }
    }

    /// Единственная страница, которую может показывать браузер интерфейса.
    fn ui_url(self) -> Option<&'static str> {
        match self {
            Role::Toolbar => Some(ui::toolbar_url()),
            Role::Popup => Some(ui::popup_url()),
            Role::Splitter => Some(ui::splitter_url()),
            Role::Tab(_) | Role::DevTools(_) => None,
        }
    }
}

pub fn new_client(shell: Arc<Shell>, role: Role) -> Client {
    let handlers = Handlers {
        display: RustiumDisplayHandler::new(shell.clone(), role),
        load: RustiumLoadHandler::new(shell.clone(), role),
        life_span: RustiumLifeSpanHandler::new(shell.clone(), role),
        request: RustiumRequestHandler::new(shell.clone(), role),
        keyboard: RustiumKeyboardHandler::new(shell.clone()),
        drag: RustiumDragHandler::new(shell.clone(), role),
        download: RustiumDownloadHandler::new(shell.clone()),
        permission: RustiumPermissionHandler::new(shell.clone(), role),
    };
    RustiumClient::new(shell, role, handlers)
}

/// Обработчики создаются один раз на браузер и отдаются CEF по запросу.
#[derive(Clone)]
struct Handlers {
    display: DisplayHandler,
    load: LoadHandler,
    life_span: LifeSpanHandler,
    request: RequestHandler,
    keyboard: KeyboardHandler,
    drag: DragHandler,
    download: DownloadHandler,
    permission: PermissionHandler,
}

wrap_client! {
    pub struct RustiumClient {
        shell: Arc<Shell>,
        role: Role,
        handlers: Handlers,
    }

    impl Client {
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.handlers.display.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.handlers.load.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.handlers.life_span.clone())
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.handlers.request.clone())
        }

        fn keyboard_handler(&self) -> Option<KeyboardHandler> {
            Some(self.handlers.keyboard.clone())
        }

        fn drag_handler(&self) -> Option<DragHandler> {
            Some(self.handlers.drag.clone())
        }

        fn permission_handler(&self) -> Option<PermissionHandler> {
            // Разрешения просят только сайты во вкладках.
            matches!(self.role, Role::Tab(_)).then(|| self.handlers.permission.clone())
        }

        fn download_handler(&self) -> Option<DownloadHandler> {
            // Скачивать можно только из вкладок.
            matches!(self.role, Role::Tab(_)).then(|| self.handlers.download.clone())
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            // IPC-сообщения message router'а приходят от интерфейса и от
            // служебных страниц rustium:// во вкладках (их проверяет ipc.rs).
            if !(self.role.is_ui() || matches!(self.role, Role::Tab(_))) {
                return 0;
            }
            let Some(router) = self.shell.router() else {
                return 0;
            };
            router.on_process_message_received(
                browser.cloned(),
                frame.cloned(),
                source_process,
                message.cloned(),
            ) as c_int
        }
    }
}

// -------------------------------------------------------------------------
// Заголовок, адрес, favicon, прогресс
// -------------------------------------------------------------------------

wrap_display_handler! {
    struct RustiumDisplayHandler {
        shell: Arc<Shell>,
        role: Role,
    }

    impl DisplayHandler {
        fn on_address_change(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            url: Option<&CefString>,
        ) {
            let role = self.shell.role_of(self.role, browser.as_deref());
            let Role::Tab(id) = role else { return };
            if frame.is_none_or(|f| f.is_main() == 0) {
                return;
            }
            let url = url.map(|u| u.to_string()).unwrap_or_default();
            // Запросы разрешений другого сайта больше не нужны; звук —
            // по настройкам нового сайта.
            self.shell.drop_prompts(id, Some(&url));
            if let Some(browser) = browser.as_deref() {
                self.shell.apply_sound(browser, &url);
            }
            self.shell.update_tab(id, |tab| {
                // Ушли со страницы ошибки — забываем её.
                if tab.error_page.is_some() && !ui::is_error_page(&url) {
                    tab.error_page = None;
                    tab.cert_error = false;
                }
                tab.url = url;
                tab.favicon = None;
            });
        }

        fn on_media_access_change(
            &self,
            browser: Option<&mut Browser>,
            has_video_access: ::std::os::raw::c_int,
            has_audio_access: ::std::os::raw::c_int,
        ) {
            let role = self.shell.role_of(self.role, browser.as_deref());
            let Role::Tab(id) = role else { return };
            self.shell
                .media_access_changed(id, has_video_access != 0, has_audio_access != 0);
        }

        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            let role = self.shell.role_of(self.role, browser.as_deref());
            let Role::Tab(id) = role else { return };
            let title = title.map(|t| t.to_string()).unwrap_or_default();
            self.shell.update_tab(id, |tab| tab.title = title);
        }

        fn on_favicon_urlchange(
            &self,
            browser: Option<&mut Browser>,
            icon_urls: Option<&mut CefStringList>,
        ) {
            let role = self.shell.role_of(self.role, browser.as_deref());
            let Role::Tab(id) = role else { return };
            let icon = first_string(icon_urls).filter(|u| {
                u.starts_with("https://") || u.starts_with("http://") || u.starts_with("data:image/")
            });
            self.shell.update_tab(id, |tab| tab.favicon = icon);
        }

        fn on_loading_progress_change(&self, browser: Option<&mut Browser>, progress: f64) {
            let role = self.shell.role_of(self.role, browser.as_deref());
            let Role::Tab(id) = role else { return };
            self.shell.update_tab(id, |tab| tab.progress = progress);
        }

        fn on_console_message(
            &self,
            _browser: Option<&mut Browser>,
            _level: LogSeverity,
            message: Option<&CefString>,
            source: Option<&CefString>,
            line: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            // Ошибки JS интерфейса печатаем в терминал — так их проще отлаживать.
            if self.role.is_ui() {
                let message = message.map(|m| m.to_string()).unwrap_or_default();
                let source = source.map(|s| s.to_string()).unwrap_or_default();
                let source = if source.starts_with("data:") { "html".to_string() } else { source };
                eprintln!("{} {source}:{line}: {message}", self.role.log_prefix());
            }
            0
        }
    }
}

/// Первая строка из списка CEF без копирования самого списка.
fn first_string(list: Option<&mut CefStringList>) -> Option<String> {
    let list = list?;
    let raw: *mut cef::sys::_cef_string_list_t = list.into();
    // Обёртка над тем же указателем; CefStringList освобождает память
    // только у списков, созданных через `new()`, так что это безопасно.
    CefStringList::from(raw).into_iter().next()
}

// -------------------------------------------------------------------------
// Загрузка
// -------------------------------------------------------------------------

wrap_load_handler! {
    struct RustiumLoadHandler {
        shell: Arc<Shell>,
        role: Role,
    }

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            can_go_back: ::std::os::raw::c_int,
            can_go_forward: ::std::os::raw::c_int,
        ) {
            let role = self.shell.role_of(self.role, browser.as_deref());
            let Role::Tab(id) = role else { return };
            self.shell.update_tab(id, |tab| {
                tab.loading = is_loading != 0;
                tab.can_go_back = can_go_back != 0;
                tab.can_go_forward = can_go_forward != 0;
                if is_loading == 0 {
                    tab.progress = 1.0;
                }
            });
            // Новый сайт мог открыться со своим масштабом.
            if is_loading == 0 {
                self.shell.refresh_zoom();
            }
        }

        fn on_load_error(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            let role = self.shell.role_of(self.role, browser.as_deref());
            let Role::Tab(id) = role else { return };
            let Some(frame) = frame else { return };
            // ERR_ABORTED — пользователь сам остановил загрузку или ушёл
            // со страницы; это не ошибка.
            if frame.is_main() == 0 || error_code.get_raw() == Errorcode::ABORTED.get_raw() {
                return;
            }
            let text = error_text.map(|t| t.to_string()).unwrap_or_default();
            let failed_url = failed_url.map(|u| u.to_string()).unwrap_or_default();
            // Не зацикливаемся, если не открылась сама служебная страница.
            if failed_url.starts_with("rustium://") {
                return;
            }
            let code = error_code.get_raw();
            let code_name = if text.is_empty() { format!("ERR_{}", -code) } else { text };
            self.shell.show_load_error(id, frame, code, &code_name, &failed_url);
        }
    }
}

// -------------------------------------------------------------------------
// Жизненный цикл браузера и всплывающие окна
// -------------------------------------------------------------------------

wrap_life_span_handler! {
    struct RustiumLifeSpanHandler {
        shell: Arc<Shell>,
        role: Role,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            let role = self.shell.role_of(self.role, browser.as_deref());
            // target=_blank и window.open() открываем вкладкой, а не окном.
            // Из панели всплывающие окна не открываем вообще.
            if let Role::Tab(id) = role {
                // Окна без действия пользователя (реклама, редиректы)
                // открываем, только если сайту это разрешено.
                let opener = browser
                    .and_then(|b| b.main_frame())
                    .map(|f| CefString::from(&f.url()).to_string())
                    .unwrap_or_default();
                let allowed = user_gesture != 0
                    || settings::decide(PermKind::Popups, &opener) == Setting::Allow;
                if allowed {
                    let url = target_url.map(|u| u.to_string()).unwrap_or_default();
                    open_in_new_tab(&self.shell, id, url, target_disposition);
                } else {
                    eprintln!("rustium: заблокировано всплывающее окно с {opener}");
                }
            }
            1
        }

        fn on_after_created(&self, _browser: Option<&mut Browser>) {
            self.shell.browser_created();
        }

        fn do_close(&self, browser: Option<&mut Browser>) -> ::std::os::raw::c_int {
            let role = self.shell.role_of(self.role, browser.as_deref());
            self.shell.do_close(role)
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            let role = self.shell.role_of(self.role, browser.as_deref());
            self.shell.browser_closed(role, browser.cloned());
        }
    }
}

/// Имя причины падения renderer для страницы «вкладка упала».
fn termination_name(status: TerminationStatus) -> &'static str {
    let raw = status.get_raw();
    if raw == TerminationStatus::PROCESS_WAS_KILLED.get_raw() {
        "RESULT_CODE_KILLED"
    } else if raw == TerminationStatus::PROCESS_CRASHED.get_raw() {
        "RESULT_CODE_CRASHED"
    } else if raw == TerminationStatus::PROCESS_OOM.get_raw() {
        "RESULT_CODE_OUT_OF_MEMORY"
    } else if raw == TerminationStatus::LAUNCH_FAILED.get_raw() {
        "RESULT_CODE_LAUNCH_FAILED"
    } else if raw == TerminationStatus::INTEGRITY_FAILURE.get_raw() {
        "RESULT_CODE_INTEGRITY_FAILURE"
    } else {
        "RESULT_CODE_ABNORMAL_TERMINATION"
    }
}

fn open_in_new_tab(
    shell: &Arc<Shell>,
    opener: TabId,
    url: String,
    disposition: WindowOpenDisposition,
) {
    if url.is_empty() {
        return;
    }
    let foreground = disposition.get_raw() != WindowOpenDisposition::NEW_BACKGROUND_TAB.get_raw();
    let shell = shell.clone();
    post_ui(move || {
        shell.perform(Action::OpenUrl {
            url,
            opener: Some(opener),
            foreground,
        })
    });
}

// -------------------------------------------------------------------------
// Навигация и процессы
// -------------------------------------------------------------------------

wrap_request_handler! {
    struct RustiumRequestHandler {
        shell: Arc<Shell>,
        role: Role,
    }

    impl RequestHandler {
        fn on_before_browse(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: ::std::os::raw::c_int,
            _is_redirect: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let Some(allowed) = self.role.ui_url() else {
                // Вкладка уходит со страницы: незавершённые cefQuery
                // служебной страницы отменяются.
                if matches!(self.role, Role::Tab(_)) {
                    if let Some(router) = self.shell.router() {
                        router.on_before_browse(browser.cloned(), frame.cloned());
                    }
                }
                return 0;
            };
            // Интерфейс может показывать только сам себя: никакие ссылки,
            // перетащенные файлы и т.п. не должны увести его на сайт,
            // у которого появился бы доступ к cefQuery.
            let url = request.map(|r| CefString::from(&r.url()).to_string()).unwrap_or_default();
            if url != allowed {
                let shown: String = url.chars().take(80).collect();
                eprintln!("{} навигация заблокирована: {shown}", self.role.log_prefix());
                return 1;
            }
            if let Some(router) = self.shell.router() {
                router.on_before_browse(browser.cloned(), frame.cloned());
            }
            0
        }

        // Плохой сертификат: если пользователь уже выбрал «Перейти
        // небезопасно» для этого хоста — продолжаем, иначе отказ и наша
        // страница ошибки (on_load_error) с данными сертификата.
        fn on_certificate_error(
            &self,
            browser: Option<&mut Browser>,
            _cert_error: Errorcode,
            request_url: Option<&CefString>,
            ssl_info: Option<&mut Sslinfo>,
            callback: Option<&mut Callback>,
        ) -> ::std::os::raw::c_int {
            let role = self.shell.role_of(self.role, browser.as_deref());
            let Role::Tab(id) = role else { return 0 };
            let url = request_url.map(|u| u.to_string()).unwrap_or_default();
            let cert = ssl_info
                .and_then(|info| info.x509_certificate())
                .map(|cert| ui::CertInfo {
                    issuer: cert
                        .issuer()
                        .map(|p| CefString::from(&p.display_name()).to_string())
                        .unwrap_or_default(),
                    valid_from: crate::scheme::basetime_to_unix(cert.valid_start().val),
                    valid_to: crate::scheme::basetime_to_unix(cert.valid_expiry().val),
                })
                .unwrap_or_default();
            if self.shell.certificate_error(id, &url, cert) {
                if let Some(callback) = callback {
                    callback.cont();
                    return 1;
                }
            }
            0
        }

        fn on_open_urlfrom_tab(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            target_url: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let role = self.shell.role_of(self.role, browser.as_deref());
            // Ctrl/Cmd+клик и средняя кнопка по ссылке.
            if let Role::Tab(id) = role {
                let url = target_url.map(|u| u.to_string()).unwrap_or_default();
                open_in_new_tab(&self.shell, id, url, target_disposition);
            }
            1
        }

        fn on_render_process_terminated(
            &self,
            browser: Option<&mut Browser>,
            status: TerminationStatus,
            error_code: ::std::os::raw::c_int,
            error_string: Option<&CefString>,
        ) {
            let role = self.shell.role_of(self.role, browser.as_deref());
            match role {
                Role::Toolbar | Role::Popup | Role::Splitter => {
                    if let Some(router) = self.shell.router() {
                        router.on_render_process_terminated(browser.as_deref().cloned());
                    }
                    // Перезагружаем панель, иначе браузер останется без управления.
                    if let Some(b) = browser {
                        b.reload();
                    }
                }
                Role::Tab(id) => {
                    if let Some(router) = self.shell.router() {
                        router.on_render_process_terminated(browser.as_deref().cloned());
                    }
                    let Some(b) = browser else { return };
                    let reason = error_string.map(|s| s.to_string()).unwrap_or_default();
                    let code_name = termination_name(status);
                    let details = if reason.is_empty() {
                        format!("exit code {error_code}")
                    } else {
                        format!("{reason} (exit code {error_code})")
                    };
                    self.shell.show_crash(id, b, code_name, &details);
                }
                // Упавшие DevTools пользователь переоткроет сам.
                Role::DevTools(_) => {}
            }
        }
    }
}

// -------------------------------------------------------------------------
// Перетаскивание: в панель ничего бросить нельзя
// -------------------------------------------------------------------------

wrap_drag_handler! {
    struct RustiumDragHandler {
        shell: Arc<Shell>,
        role: Role,
    }

    impl DragHandler {
        fn on_drag_enter(
            &self,
            _browser: Option<&mut Browser>,
            _drag_data: Option<&mut DragData>,
            _mask: DragOperationsMask,
        ) -> ::std::os::raw::c_int {
            self.role.is_ui() as c_int
        }

        // Окно без заголовка таскают за пустое место полосы вкладок:
        // панель помечает его CSS `app-region: drag`, а мы передаём
        // эти области окну.
        fn on_draggable_regions_changed(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            regions: Option<&[DraggableRegion]>,
        ) {
            if self.role == Role::Toolbar {
                self.shell.set_draggable_regions(regions.unwrap_or_default().to_vec());
            }
        }
    }
}

// -------------------------------------------------------------------------
// Загрузки: сразу в папку загрузок, без диалога (как в Chrome по умолчанию)
// -------------------------------------------------------------------------

fn snapshot(item: &DownloadItem) -> Snapshot {
    let text = |s: CefStringUserfree| CefString::from(&s).to_string();
    Snapshot {
        id: item.id(),
        path: text(item.full_path()),
        url: text(item.url()),
        suggested_name: text(item.suggested_file_name()),
        received: item.received_bytes(),
        total: item.total_bytes(),
        speed: item.current_speed(),
        in_progress: item.is_in_progress() != 0,
        paused: item.is_paused() != 0,
        complete: item.is_complete() != 0,
        canceled: item.is_canceled() != 0,
        interrupted: item.is_interrupted() != 0,
        interrupt_reason: item.interrupt_reason().get_raw(),
    }
}

wrap_download_handler! {
    struct RustiumDownloadHandler {
        shell: Arc<Shell>,
    }

    impl DownloadHandler {
        // В крейте cef метод по умолчанию возвращает 0, и CEF отменяет загрузку.
        fn can_download(
            &self,
            _browser: Option<&mut Browser>,
            _url: Option<&CefString>,
            _request_method: Option<&CefString>,
        ) -> ::std::os::raw::c_int {
            1
        }

        fn on_before_download(
            &self,
            _browser: Option<&mut Browser>,
            download_item: Option<&mut DownloadItem>,
            suggested_name: Option<&CefString>,
            callback: Option<&mut BeforeDownloadCallback>,
        ) -> ::std::os::raw::c_int {
            let (Some(item), Some(callback)) = (download_item, callback) else {
                return 0;
            };
            let download_prefs = crate::prefs::get().downloads;
            let dir = download_prefs
                .dir
                .map(std::path::PathBuf::from)
                .unwrap_or_else(downloads::downloads_dir);
            let _ = std::fs::create_dir_all(&dir);
            let name = suggested_name.map(|n| n.to_string()).unwrap_or_default();
            let path = downloads::unique_path(&dir, &name, |p| p.exists());
            let mut snap = snapshot(item);
            snap.path = path.to_string_lossy().into_owned();
            // «Спрашивать, куда сохранять»: CEF покажет диалог с этим путём.
            callback.cont(
                Some(&CefString::from(snap.path.as_str())),
                download_prefs.ask as c_int,
            );
            // Обработчики загрузок вызываются на UI-потоке, а download_started
            // не вызывает CEF под блокировкой, поэтому зовём его сразу.
            self.shell.download_started(snap);
            1
        }

        fn on_download_updated(
            &self,
            _browser: Option<&mut Browser>,
            download_item: Option<&mut DownloadItem>,
            callback: Option<&mut DownloadItemCallback>,
        ) {
            let Some(item) = download_item else { return };
            let snap = snapshot(item);
            let callback = callback.map(|c| c.clone());
            self.shell.download_updated(snap, callback);
        }
    }
}

// -------------------------------------------------------------------------
// Разрешения сайтов (решения — в shell/permissions.rs)
// -------------------------------------------------------------------------

wrap_permission_handler! {
    struct RustiumPermissionHandler {
        shell: Arc<Shell>,
        role: Role,
    }

    impl PermissionHandler {
        fn on_request_media_access_permission(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            requesting_origin: Option<&CefString>,
            requested_permissions: u32,
            callback: Option<&mut MediaAccessCallback>,
        ) -> ::std::os::raw::c_int {
            let role = self.shell.role_of(self.role, browser.as_deref());
            let (Role::Tab(id), Some(callback)) = (role, callback) else {
                return 0;
            };
            let origin = requesting_origin.map(|o| o.to_string()).unwrap_or_default();
            self.shell
                .media_request(id, origin, requested_permissions, callback.clone()) as c_int
        }

        fn on_show_permission_prompt(
            &self,
            browser: Option<&mut Browser>,
            prompt_id: u64,
            requesting_origin: Option<&CefString>,
            requested_permissions: u32,
            callback: Option<&mut PermissionPromptCallback>,
        ) -> ::std::os::raw::c_int {
            let role = self.shell.role_of(self.role, browser.as_deref());
            let (Role::Tab(id), Some(callback)) = (role, callback) else {
                return 0;
            };
            let origin = requesting_origin.map(|o| o.to_string()).unwrap_or_default();
            self.shell.permission_prompt(
                id,
                prompt_id,
                origin,
                requested_permissions,
                callback.clone(),
            ) as c_int
        }

        fn on_dismiss_permission_prompt(
            &self,
            _browser: Option<&mut Browser>,
            prompt_id: u64,
            _result: PermissionRequestResult,
        ) {
            self.shell.permission_dismissed(prompt_id);
        }
    }
}

// -------------------------------------------------------------------------
// Горячие клавиши
// -------------------------------------------------------------------------

// Сигнатура on_pre_key_event отличается по платформам (тип нативного
// события), поэтому обёртка объявлена трижды, а логика — общая.

#[cfg(target_os = "linux")]
wrap_keyboard_handler! {
    struct RustiumKeyboardHandler {
        shell: Arc<Shell>,
    }

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            browser: Option<&mut Browser>,
            event: Option<&KeyEvent>,
            _os_event: Option<&mut cef::sys::XEvent>,
            _is_keyboard_shortcut: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            crate::shortcuts::handle(&self.shell, browser, event) as c_int
        }
    }
}

#[cfg(target_os = "macos")]
wrap_keyboard_handler! {
    struct RustiumKeyboardHandler {
        shell: Arc<Shell>,
    }

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            browser: Option<&mut Browser>,
            event: Option<&KeyEvent>,
            _os_event: *mut u8,
            _is_keyboard_shortcut: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            crate::shortcuts::handle(&self.shell, browser, event) as c_int
        }
    }
}

#[cfg(target_os = "windows")]
wrap_keyboard_handler! {
    struct RustiumKeyboardHandler {
        shell: Arc<Shell>,
    }

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            browser: Option<&mut Browser>,
            event: Option<&KeyEvent>,
            _os_event: Option<&mut cef::sys::MSG>,
            _is_keyboard_shortcut: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            crate::shortcuts::handle(&self.shell, browser, event) as c_int
        }
    }
}
