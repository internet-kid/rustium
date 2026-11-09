//! Состояние браузера (окно, панель, вкладки) и все действия над ним.
//!
//! Правило для всего модуля: блокировка `state` никогда не удерживается
//! во время вызовов CEF. CEF может синхронно вызвать наши же обработчики
//! (например, `on_browser_created` внутри `add_child_view`), и они снова
//! захотят взять блокировку. Поэтому везде схема одна: взять блокировку,
//! скопировать нужное, отпустить и только потом звать CEF.

mod devtools;
mod popup;
mod transfers;

use crate::client::{self, Role};
use crate::downloads::{self, Download};
use crate::ipc::{
    Anchor, Dock, DownloadAction, PopupKind, Security, TabState, ToolbarQueryHandler, UiState,
};
use crate::task::{post_ui, post_ui_delayed};
use crate::{ui, urlfix, views};
use cef::wrapper::message_router::{
    BrowserSideRouter, MessageRouterBrowserSide, MessageRouterBrowserSideHandlerCallbacks,
    MessageRouterConfig,
};
use cef::*;
use std::collections::{HashMap, HashSet};
use std::os::raw::c_int;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Instant;

pub type TabId = u32;

/// Ключ в `extra_info` браузера панели. По нему renderer понимает,
/// куда можно внедрить `window.cefQuery`.
pub const UI_MARKER: &str = "rustium.ui";

/// Цвет фона окна и панели (ARGB), чтобы при старте не мигало белым.
pub const CHROME_BG: u32 = 0xFF_0E_0F_13;

const INITIAL_WIDTH: i32 = 1280;
const INITIAL_HEIGHT: i32 = 840;

/// Всё, что умеет браузер. Источники: панель (ipc.rs), горячие клавиши
/// (client.rs), ссылки «открыть в новой вкладке» и меню macOS.
#[derive(Debug, Clone)]
pub enum Action {
    Navigate(String),
    Back,
    Forward,
    Reload,
    Stop,
    NewTab,
    OpenUrl {
        url: String,
        opener: Option<TabId>,
        foreground: bool,
    },
    CloseTab(TabId),
    CloseActiveTab,
    SelectTab(TabId),
    SelectIndex(usize),
    SelectLast,
    NextTab,
    PrevTab,
    FocusAddress,
    FocusContent,
    DevTools,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ToggleFullscreen,
    Print,
    Quit,
    ShowPopup {
        kind: PopupKind,
        anchor: Anchor,
    },
    ClosePopup,
    OpenDownloads,
    PopupSize {
        width: f64,
        height: f64,
    },
    Download {
        id: u32,
        action: DownloadAction,
    },
    SetDevtoolsDock(Dock),
    SplitterMove {
        dx: f64,
        dy: f64,
    },
}

pub(crate) struct Tab {
    pub id: TabId,
    pub view: BrowserView,
    pub browser: Option<Browser>,
    pub title: String,
    pub url: String,
    pub favicon: Option<String>,
    pub loading: bool,
    pub progress: f64,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    /// Если показана наша страница ошибки: (её data-URL, исходный URL).
    pub error_page: Option<(String, String)>,
    /// Страница ошибки показана из-за плохого сертификата.
    pub cert_error: bool,
    /// Масштаб страницы, %.
    pub zoom: u32,
}

impl Tab {
    /// URL для адресной строки.
    fn shown_url(&self) -> &str {
        match &self.error_page {
            Some((page, failed)) if *page == self.url => failed,
            _ => ui::display_url(&self.url),
        }
    }

    /// Значок безопасности слева в адресной строке.
    fn security(&self) -> Security {
        if ui::is_newtab(&self.url) {
            return Security::None;
        }
        let on_error_page = matches!(&self.error_page, Some((page, _)) if *page == self.url);
        if on_error_page {
            return if self.cert_error { Security::CertError } else { Security::None };
        }
        Security::for_url(&self.url)
    }

    fn shown_title(&self) -> String {
        if ui::is_newtab(&self.url) {
            return ui::NEW_TAB_TITLE.to_string();
        }
        let title = self.title.trim();
        if title.is_empty() || title.starts_with("data:") {
            let url = self.shown_url();
            if url.is_empty() {
                ui::NEW_TAB_TITLE.to_string()
            } else {
                url.to_string()
            }
        } else {
            title.to_string()
        }
    }
}

#[derive(Default)]
struct State {
    window: Option<Window>,
    content: Option<Panel>,
    toolbar_view: Option<BrowserView>,
    toolbar_browser: Option<Browser>,
    toolbar_ready: bool,
    focus_address_when_ready: bool,
    tabs: Vec<Tab>,
    /// Вкладки, которые уже убраны из панели, но браузер ещё закрывается.
    closing: Vec<Tab>,
    active: Option<TabId>,
    next_id: TabId,
    window_title: String,
    window_closing: bool,
    window_destroyed: bool,
    live_browsers: usize,
    quit_requested: bool,
    fullscreen: bool,
    window_active: bool,

    // Всплывающее окно (меню, загрузки): BrowserView в overlay окна.
    popup_view: Option<BrowserView>,
    popup_overlay: Option<OverlayController>,
    popup_browser: Option<Browser>,
    popup_ready: bool,
    /// Что сейчас открыто.
    popup: Option<PopupKind>,
    popup_anchor: Option<Anchor>,
    /// Что и когда закрылось последним: повторный клик по кнопке,
    /// которая только что закрыла окно (потерей фокуса), не открывает его снова.
    popup_closed: Option<(PopupKind, Instant)>,

    /// Загрузки, новые сверху.
    downloads: Vec<Download>,
    /// Колбэки CEF для паузы и отмены активных загрузок.
    download_callbacks: HashMap<u32, DownloadItemCallback>,
    /// Список открывали после последнего завершения загрузки.
    downloads_seen: bool,
    /// Убранные из списка загрузки: их поздние события CEF игнорируем.
    removed_downloads: HashSet<u32>,

    // DevTools (shell/devtools.rs).
    dock: Dock,
    /// Встроенные в окно DevTools по вкладкам.
    devtools_views: HashMap<TabId, BrowserView>,
    /// Вкладки, у которых DevTools открыты отдельным окном.
    devtools_windows: HashSet<TabId>,
    /// Открыть DevTools этой вкладки заново, когда закроются (смена режима).
    devtools_reopen: Option<TabId>,
    devtools_width: i32,
    devtools_height: i32,
    splitter_view: Option<BrowserView>,
    splitter_browser: Option<Browser>,
    splitter_ready: bool,
    /// Браузеры встроенных DevTools (из DevToolsViewDelegate).
    devtools_browsers: HashMap<TabId, Browser>,
    /// Observer сообщений CDP вкладки для моста DevTools.
    devtools_registrations: HashMap<TabId, Registration>,
}

impl State {
    fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.tab(id))
    }

    /// Переносит вкладку из `tabs` в `closing`. Возвращает новую
    /// активную вкладку, если закрывалась активная.
    fn retire_tab(&mut self, id: TabId) -> Option<Option<TabId>> {
        let idx = self.tabs.iter().position(|t| t.id == id)?;
        let tab = self.tabs.remove(idx);
        self.closing.push(tab);
        if self.active != Some(id) {
            return Some(None);
        }
        let next = self
            .tabs
            .get(idx.min(self.tabs.len().saturating_sub(1)))
            .map(|t| t.id);
        self.active = next;
        Some(next)
    }

    fn ui_state_json(&self) -> String {
        let tabs = self
            .tabs
            .iter()
            .map(|t| TabState {
                id: t.id,
                title: t.shown_title(),
                url: t.shown_url().to_string(),
                favicon: t.favicon.clone(),
                loading: t.loading,
                progress: t.progress,
                can_go_back: t.can_go_back,
                can_go_forward: t.can_go_forward,
                security: t.security(),
            })
            .collect();
        let (downloads_badge, downloads_progress) =
            downloads::badge(&self.downloads, self.downloads_seen);
        serde_json::to_string(&UiState {
            tabs,
            active: self.active,
            fullscreen: self.fullscreen,
            window_active: self.window_active,
            zoom: self.active_tab().map_or(100, |t| t.zoom),
            downloads: self.downloads.clone(),
            downloads_badge,
            downloads_progress,
            popup: self.popup,
            devtools_dock: self.dock,
            devtools_open: self.active.is_some_and(|id| self.devtools_open(id)),
        })
        .unwrap_or_else(|_| "{}".into())
    }
}

pub struct Shell {
    weak_self: Weak<Shell>,
    state: Mutex<State>,
    router: OnceLock<Arc<BrowserSideRouter>>,
}

static GLOBAL: OnceLock<Weak<Shell>> = OnceLock::new();

/// Закрыть браузер извне (например, «Завершить» в Dock на macOS).
pub fn request_quit() {
    if let Some(shell) = GLOBAL.get().and_then(Weak::upgrade) {
        post_ui(move || shell.perform(Action::Quit));
    }
}

/// Закрытие окна отменено (на macOS об этом сообщает `cancelTerminate:`).
pub fn close_cancelled() {
    if let Some(shell) = GLOBAL.get().and_then(Weak::upgrade) {
        post_ui(move || shell.forget_cancelled_close());
    }
}

fn cef_str(s: &str) -> CefString {
    CefString::from(s)
}

pub(crate) fn browser_settings(role: Role) -> BrowserSettings {
    BrowserSettings {
        background_color: match role {
            Role::Toolbar => CHROME_BG,
            // Цвет карточки (--popover в popup.html): прозрачным окно
            // в CEF Views быть не может.
            Role::Popup => 0xFF_22_25_2D,
            // Цвет панели (--toolbar), как у разделителя в splitter.html.
            Role::Splitter => 0xFF_1D_20_27,
            Role::DevTools(_) => CHROME_BG,
            Role::Tab(_) => 0xFF_FF_FF_FF,
        },
        ..Default::default()
    }
}

impl Shell {
    pub fn new() -> Arc<Self> {
        let shell = Arc::new_cyclic(|weak| Shell {
            weak_self: weak.clone(),
            state: Mutex::new(State::default()),
            router: OnceLock::new(),
        });
        let _ = GLOBAL.set(Arc::downgrade(&shell));
        shell
    }

    fn arc(&self) -> Arc<Shell> {
        self.weak_self.upgrade().expect("Shell уже уничтожен")
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn router(&self) -> Option<Arc<BrowserSideRouter>> {
        self.router.get().cloned()
    }

    // ---------------------------------------------------------------------
    // Запуск и окно
    // ---------------------------------------------------------------------

    /// Вызывается из `on_context_initialized`: CEF готов, создаём окно.
    pub fn start(&self) {
        let router = BrowserSideRouter::new(MessageRouterConfig::default());
        router.add_handler(ToolbarQueryHandler::new(self.weak_self.clone()), false);
        let _ = self.router.set(router);

        let mut delegate = views::RustiumWindowDelegate::new(self.arc());
        // Дальше всё происходит в on_window_created.
        let _ = window_create_top_level(Some(&mut delegate));
    }

    /// Строим окно: сверху панель (HTML), под ней контейнер вкладок.
    pub(crate) fn on_window_created(&self, window: Window) {
        let layout = window.set_to_box_layout(Some(&BoxLayoutSettings {
            horizontal: 0,
            cross_axis_alignment: AxisAlignment::STRETCH,
            ..Default::default()
        }));

        // Браузер панели. Метка в extra_info разрешает ему cefQuery.
        let mut extra = dictionary_value_create();
        if let Some(extra) = extra.as_mut() {
            extra.set_bool(Some(&cef_str(UI_MARKER)), 1);
        }
        let mut toolbar_client = client::new_client(self.arc(), Role::Toolbar);
        let mut toolbar_delegate = views::ToolbarViewDelegate::new(self.arc());
        let toolbar = browser_view_create(
            Some(&mut toolbar_client),
            Some(&cef_str(ui::toolbar_url())),
            Some(&browser_settings(Role::Toolbar)),
            extra.as_mut(),
            None,
            Some(&mut toolbar_delegate),
        );

        // Контейнер вкладок: все BrowserView лежат в нём друг на друге,
        // видима только активная. Layout-менеджера нет: размеры страниц
        // и встроенных DevTools считает layout_content (shell/devtools.rs).
        let mut content_delegate = views::ContentPanelDelegate::new(self.arc());
        let content = panel_create(Some(&mut content_delegate));
        if let Some(content) = &content {
            content.set_background_color(0xFF_FF_FF_FF);
        }

        {
            let mut s = self.lock();
            s.window = Some(window.clone());
            s.window_active = true;
            s.devtools_width = devtools::DEFAULT_WIDTH;
            s.devtools_height = devtools::DEFAULT_HEIGHT;
            s.toolbar_view = toolbar.clone();
            s.content = content.clone();
        }

        // Опыт chrome-tab: в окне Chrome-стиля вкладка в Chrome-стиле должна
        // попасть в окно раньше любого Alloy BrowserView (CEF
        // browser_view_impl.cc AddedToWidget), поэтому сначала панель
        // вкладок и первая вкладка, а панель инструментов — потом, наверх.
        let chrome_tab = views::chrome_tab_experiment();
        if !chrome_tab {
            if let Some(toolbar) = &toolbar {
                let mut view = View::from(toolbar);
                window.add_child_view(Some(&mut view));
                if let Some(layout) = &layout {
                    layout.set_flex_for_view(Some(&mut view), 0);
                }
            }
        }
        if let Some(content) = &content {
            let mut view = View::from(content);
            window.add_child_view(Some(&mut view));
            if let Some(layout) = &layout {
                layout.set_flex_for_view(Some(&mut view), 1);
            }
        }
        if chrome_tab {
            self.open_tab(None, None, true);
            if let Some(toolbar) = &toolbar {
                let mut view = View::from(toolbar);
                window.add_child_view_at(Some(&mut view), 0);
                if let Some(layout) = &layout {
                    layout.set_flex_for_view(Some(&mut view), 0);
                }
            }
        }

        window.set_background_color(CHROME_BG);
        window.set_title(Some(&cef_str("Rustium")));
        window.center_window(Some(&Size {
            width: INITIAL_WIDTH,
            height: INITIAL_HEIGHT,
        }));

        self.create_popup(&window);
        if let Some(content) = &content {
            self.create_splitter(content);
        }
        if !chrome_tab {
            self.open_tab(None, None, true);
        }
        window.show();
        window.activate();
    }

    /// Пользователь закрывает окно. Сначала даём закрыться браузерам
    /// (CEF выполнит unload-обработчики), окно закроется при повторном
    /// вызове, когда все браузеры будут готовы.
    pub(crate) fn can_close_window(&self) -> bool {

        let browsers: Vec<Browser> = {
            let mut s = self.lock();
            s.window_closing = true;
            s.toolbar_browser
                .iter()
                .cloned()
                .chain(s.popup_browser.iter().cloned())
                .chain(s.splitter_browser.iter().cloned())
                .chain(s.devtools_browsers.values().cloned())
                .chain(s.tabs.iter().filter_map(|t| t.browser.clone()))
                .chain(s.closing.iter().filter_map(|t| t.browser.clone()))
                .collect()
        };
        let mut can_close = true;
        for browser in browsers {
            if let Some(host) = browser.host() {
                // Окна DevTools — отдельные браузеры; закрываем их вместе
                // с окном, иначе CEF остановится с открытым браузером.
                if host.has_dev_tools() != 0 {
                    host.close_dev_tools();
                }
                if host.try_close_browser() == 0 {
                    can_close = false;
                }
            }
        }
        can_close
    }

    /// Если на странице спросили «Покинуть сайт?» и пользователь нажал
    /// «Остаться», окно не закрывается, а CEF об этом не сообщает.
    /// Поэтому флаг `window_closing` сбрасываем при следующем действии
    /// пользователя: раз он снова работает с окном, закрытие отменено.
    /// Если закрытие на самом деле продолжается, `can_close_window`
    /// вызовется ещё раз и снова поставит флаг.
    fn forget_cancelled_close(&self) {
        let mut s = self.lock();
        if s.window_closing && !s.window_destroyed {
            s.window_closing = false;
        }
    }

    pub(crate) fn on_window_destroyed(&self) {
        let (leftovers, quit_now) = {
            let mut s = self.lock();
            s.window_destroyed = true;
            let leftovers = (
                s.window.take(),
                s.content.take(),
                s.toolbar_view.take(),
                (s.popup_view.take(), s.popup_overlay.take()),
                (s.splitter_view.take(), std::mem::take(&mut s.devtools_views)),
                std::mem::take(&mut s.devtools_registrations),
                std::mem::take(&mut s.tabs),
                std::mem::take(&mut s.closing),
                std::mem::take(&mut s.download_callbacks),
            );
            (leftovers, s.live_browsers == 0)
        };
        drop(leftovers);
        if quit_now {
            self.quit();
        } else {
            // Страховка: если какой-то браузер так и не сообщил о закрытии.
            let shell = self.arc();
            post_ui_delayed(3000, move || shell.quit());
        }
    }

    fn quit(&self) {
        {
            let mut s = self.lock();
            if s.quit_requested {
                return;
            }
            s.quit_requested = true;
        }
        quit_message_loop();
    }

    // ---------------------------------------------------------------------
    // Действия
    // ---------------------------------------------------------------------

    pub fn perform(&self, action: Action) {
        if !matches!(action, Action::Quit) {
            self.forget_cancelled_close();
        }
        // Любое действие, кроме работы с самим всплывающим окном,
        // масштаба и загрузок, его закрывает.
        let keeps_popup = matches!(
            action,
            Action::ShowPopup { .. }
                | Action::ClosePopup
                | Action::PopupSize { .. }
                | Action::ZoomIn
                | Action::ZoomOut
                | Action::ZoomReset
                | Action::Download { .. }
                | Action::SetDevtoolsDock(_)
        );
        if !keeps_popup {
            self.close_popup(false);
        }
        match action {
            Action::Navigate(text) => self.navigate(&text),
            Action::Back => self.with_active_browser(|b| b.go_back()),
            Action::Forward => self.with_active_browser(|b| b.go_forward()),
            Action::Reload => self.reload(),
            Action::Stop => self.with_active_browser(|b| b.stop_load()),
            Action::NewTab => {
                self.open_tab(None, None, true);
            }
            Action::OpenUrl {
                url,
                opener,
                foreground,
            } => {
                self.open_tab(Some(url), opener, foreground);
            }
            Action::CloseTab(id) => self.close_tab(id),
            Action::CloseActiveTab => {
                let active = self.lock().active;
                if let Some(id) = active {
                    self.close_tab(id);
                }
            }
            Action::SelectTab(id) => self.activate(id),
            Action::SelectIndex(index) => {
                let id = self.lock().tabs.get(index).map(|t| t.id);
                if let Some(id) = id {
                    self.activate(id);
                }
            }
            Action::SelectLast => {
                let id = self.lock().tabs.last().map(|t| t.id);
                if let Some(id) = id {
                    self.activate(id);
                }
            }
            Action::NextTab | Action::PrevTab => {
                let forward = matches!(action, Action::NextTab);
                let id = {
                    let s = self.lock();
                    let n = s.tabs.len();
                    let cur = s.tabs.iter().position(|t| Some(t.id) == s.active);
                    match (n, cur) {
                        (0, _) | (_, None) => None,
                        (n, Some(i)) => {
                            let j = if forward {
                                (i + 1) % n
                            } else {
                                (i + n - 1) % n
                            };
                            Some(s.tabs[j].id)
                        }
                    }
                };
                if let Some(id) = id {
                    self.activate(id);
                }
            }
            Action::FocusAddress => self.focus_address(),
            Action::FocusContent => self.focus_content(),
            Action::DevTools => self.toggle_devtools(),
            Action::SetDevtoolsDock(dock) => self.set_devtools_dock(dock),
            Action::SplitterMove { dx, dy } => self.splitter_move(dx, dy),
            Action::ZoomIn => self.zoom(ZoomCommand::IN),
            Action::ZoomOut => self.zoom(ZoomCommand::OUT),
            Action::ZoomReset => self.zoom(ZoomCommand::RESET),
            Action::ToggleFullscreen => {
                let window = self.lock().window.clone();
                if let Some(window) = window {
                    window.set_fullscreen((window.is_fullscreen() == 0) as c_int);
                }
            }
            Action::Print => self.with_active_browser(|b| {
                if let Some(host) = b.host() {
                    host.print();
                }
            }),
            Action::ShowPopup { kind, anchor } => self.show_popup(kind, anchor),
            Action::ClosePopup => self.close_popup(true),
            // Кнопка загрузок в панели знает своё место: панель сама
            // пришлёт showPopup с её координатами.
            Action::OpenDownloads => {
                self.run_in_toolbar("window.rustium && window.rustium.openDownloads();")
            }
            Action::PopupSize { width, height } => self.popup_size(width, height),
            Action::Download { id, action } => self.download_action(id, action),
            Action::Quit => {
                let window = self.lock().window.clone();
                if let Some(window) = window {
                    window.close();
                }
            }
        }
    }

    fn active_browser(&self) -> Option<Browser> {
        self.lock().active_tab().and_then(|t| t.browser.clone())
    }

    fn with_active_browser(&self, f: impl FnOnce(&Browser)) {
        if let Some(browser) = self.active_browser() {
            f(&browser);
        }
    }

    fn zoom(&self, command: ZoomCommand) {
        self.with_active_browser(|b| {
            if let Some(host) = b.host() {
                host.zoom(command);
            }
        });
        // Уровень меняется асинхронно: читаем его чуть позже.
        let shell = self.arc();
        post_ui_delayed(60, move || shell.refresh_zoom());
    }

    /// Прочитать масштаб активной вкладки и обновить панель.
    pub(crate) fn refresh_zoom(&self) {
        let (id, browser) = {
            let s = self.lock();
            match s.active_tab() {
                Some(t) => (t.id, t.browser.clone()),
                None => return,
            }
        };
        let Some(host) = browser.and_then(|b| b.host()) else { return };
        let percent = zoom_percent(host.zoom_level());
        self.update_tab(id, |tab| tab.zoom = percent);
    }

    fn navigate(&self, text: &str) {
        let Some(url) = urlfix::fixup(text) else {
            return;
        };
        self.load_in_active(&url);
        self.focus_content();
    }

    fn load_in_active(&self, url: &str) {
        self.with_active_browser(|b| {
            if let Some(frame) = b.main_frame() {
                frame.load_url(Some(&cef_str(url)));
            }
        });
    }

    /// На странице ошибки «обновить» = повторить исходный адрес.
    fn reload(&self) {
        let retry = {
            let s = self.lock();
            s.active_tab().and_then(|t| match &t.error_page {
                Some((page, failed)) if *page == t.url => Some(failed.clone()),
                _ => None,
            })
        };
        match retry {
            Some(url) => self.load_in_active(&url),
            None => self.with_active_browser(|b| b.reload()),
        }
    }

    /// Новая вкладка. `url = None` — служебная страница новой вкладки.
    fn open_tab(
        &self,
        url: Option<String>,
        opener: Option<TabId>,
        foreground: bool,
    ) -> Option<TabId> {
        let is_newtab = url.is_none();
        let url = url.unwrap_or_else(|| ui::newtab_url().to_string());
        let id = {
            let mut s = self.lock();
            if s.window_closing || s.window_destroyed {
                return None;
            }
            s.next_id += 1;
            s.next_id
        };

        let mut client = client::new_client(self.arc(), Role::Tab(id));
        let mut delegate = views::TabViewDelegate::new(self.arc(), id);
        let view = browser_view_create(
            Some(&mut client),
            Some(&cef_str(&url)),
            Some(&browser_settings(Role::Tab(id))),
            None,
            None,
            Some(&mut delegate),
        )?;

        let content = {
            let mut s = self.lock();
            let pos = opener
                .and_then(|o| s.tabs.iter().position(|t| t.id == o))
                .map(|i| i + 1)
                .unwrap_or(s.tabs.len());
            s.tabs.insert(
                pos,
                Tab {
                    id,
                    view: view.clone(),
                    browser: None,
                    title: String::new(),
                    url,
                    favicon: None,
                    loading: true,
                    progress: 0.0,
                    can_go_back: false,
                    can_go_forward: false,
                    error_page: None,
                    cert_error: false,
                    zoom: 100,
                },
            );
            s.content.clone()
        };
        if let Some(content) = content {
            content.add_child_view(Some(&mut View::from(&view)));
        }

        if foreground {
            self.activate(id);
            if is_newtab {
                self.focus_address();
            }
        } else {
            view.set_visible(0);
            self.push_state();
        }
        Some(id)
    }

    fn activate(&self, id: TabId) {
        let (views, focus_view) = {
            let mut s = self.lock();
            let Some(tab) = s.tab(id) else {
                return;
            };
            let focus_view = (!ui::is_newtab(&tab.url)).then(|| tab.view.clone());
            s.active = Some(id);
            let views: Vec<(bool, BrowserView)> = s
                .tabs
                .iter()
                .map(|t| (t.id == id, t.view.clone()))
                .collect();
            (views, focus_view)
        };
        // Сначала показываем новую, потом прячем остальные — без «пустого» кадра.
        for (_, view) in views.iter().filter(|(active, _)| *active) {
            view.set_visible(1);
        }
        for (_, view) in views.iter().filter(|(active, _)| !*active) {
            view.set_visible(0);
        }
        // Размеры страниц, встроенных DevTools и разделителя.
        self.layout_content();
        if let Some(view) = focus_view {
            view.request_focus();
        }
        self.push_state();
        // Масштаб в Chromium хранится по сайту: у другой вкладки он свой.
        self.refresh_zoom();
    }

    fn close_tab(&self, id: TabId) {
        enum Plan {
            CloseWindow(Option<Window>),
            CloseTab {
                browser: Option<Browser>,
                view: BrowserView,
                next: Option<TabId>,
            },
        }
        let plan = {
            let mut s = self.lock();
            if s.tab(id).is_none() {
                return;
            }
            if s.tabs.len() == 1 {
                // Как в Chrome: закрыли последнюю вкладку — закрылось окно.
                Plan::CloseWindow(s.window.clone())
            } else {
                let next = s.retire_tab(id).flatten();
                let tab = s.closing.last().expect("только что добавили");
                Plan::CloseTab {
                    browser: tab.browser.clone(),
                    view: tab.view.clone(),
                    next,
                }
            }
        };

        match plan {
            Plan::CloseWindow(window) => {
                if let Some(window) = window {
                    window.close();
                }
            }
            Plan::CloseTab {
                browser,
                view,
                next,
            } => {
                // Встроенные DevTools закрываем вместе со вкладкой.
                self.close_devtools_of(id);
                view.set_visible(0);
                match next {
                    Some(next) => self.activate(next),
                    None => self.push_state(),
                }
                match browser.and_then(|b| b.host()) {
                    // force_close = 1: без диалога «Покинуть сайт?».
                    // Дальше CEF вызовет do_close -> detach_tab.
                    Some(host) => host.close_browser(1),
                    None => self.detach_tab(id),
                }
            }
        }
    }

    /// Убираем BrowserView закрываемой вкладки из окна и отпускаем ссылки:
    /// CEF уничтожит view, затем браузер, затем вызовет on_before_close.
    fn detach_tab(&self, id: TabId) {
        let (content, tab) = {
            let mut s = self.lock();
            let idx = s.closing.iter().position(|t| t.id == id);
            (s.content.clone(), idx.map(|i| s.closing.remove(i)))
        };
        let Some(tab) = tab else {
            return;
        };
        if let Some(content) = content {
            content.remove_child_view(Some(&mut View::from(&tab.view)));
        }
        drop(tab);
    }

    // ---------------------------------------------------------------------
    // Фокус и панель
    // ---------------------------------------------------------------------

    fn focus_address(&self) {
        let (view, ready) = {
            let mut s = self.lock();
            if !s.toolbar_ready {
                s.focus_address_when_ready = true;
            }
            (s.toolbar_view.clone(), s.toolbar_ready)
        };
        if let Some(view) = view {
            view.request_focus();
        }
        if ready {
            self.run_in_toolbar("window.rustium && window.rustium.focusAddress();");
        }
    }

    fn focus_content(&self) {
        let view = self.lock().active_tab().map(|t| t.view.clone());
        if let Some(view) = view {
            view.request_focus();
        }
    }

    /// Панель или всплывающее окно загрузились (команда `ready`):
    /// отдаём им состояние.
    pub(crate) fn ui_ready(&self, browser: &Browser) -> String {
        if self.is_popup(browser) {
            let mut s = self.lock();
            s.popup_ready = true;
            return s.ui_state_json();
        }
        if self.is_splitter(browser) {
            let mut s = self.lock();
            s.splitter_ready = true;
            return s.ui_state_json();
        }
        let (json, focus) = {
            let mut s = self.lock();
            s.toolbar_ready = true;
            let focus = std::mem::take(&mut s.focus_address_when_ready);
            (s.ui_state_json(), focus)
        };
        if focus {
            let shell = self.arc();
            post_ui(move || shell.focus_address());
        }
        json
    }

    /// Отправить панели и всплывающему окну свежее состояние
    /// и обновить заголовок окна.
    pub(crate) fn push_state(&self) {
        let (json, popup_json, splitter_json, toolbar, window_title) = {
            let mut s = self.lock();
            let title = s.active_tab().map(|t| t.shown_title()).unwrap_or_default();
            let window_title = if title.is_empty() {
                "Rustium".to_string()
            } else {
                format!("{title} — Rustium")
            };
            let changed = (window_title != s.window_title).then(|| {
                s.window_title = window_title.clone();
                (s.window.clone(), window_title)
            });
            let json = s.toolbar_ready.then(|| s.ui_state_json());
            // Всплывающему окну шлём и закрытие (popup: null), чтобы при
            // следующем открытии оно заново измерило себя.
            let popup_json = s.popup_ready.then(|| s.ui_state_json());
            // Разделителю нужно только положение DevTools.
            let splitter_json = s.splitter_ready.then(|| {
                serde_json::json!({ "devtoolsDock": s.dock }).to_string()
            });
            (json, popup_json, splitter_json, s.toolbar_browser.clone(), changed)
        };
        if let Some((Some(window), title)) = window_title {
            window.set_title(Some(&cef_str(&title)));
        }
        if let (Some(json), Some(_)) = (json, toolbar) {
            self.run_in_toolbar(&format!("window.rustium && window.rustium.update({json});"));
        }
        if let Some(json) = popup_json {
            self.run_in_popup(&format!("window.rustium && window.rustium.update({json});"));
        }
        if let Some(json) = splitter_json {
            self.run_in_splitter(&format!("window.rustium && window.rustium.update({json});"));
        }
    }

    pub(crate) fn run_in_toolbar(&self, code: &str) {
        let browser = self.lock().toolbar_browser.clone();
        if let Some(frame) = browser.and_then(|b| b.main_frame()) {
            frame.execute_java_script(Some(&cef_str(code)), Some(&cef_str("rustium://toolbar")), 0);
        }
    }

    // ---------------------------------------------------------------------
    // События от обработчиков CEF (client.rs, views.rs)
    // ---------------------------------------------------------------------

    pub(crate) fn is_toolbar(&self, browser: &Browser) -> bool {
        let id = browser.identifier();
        self.lock()
            .toolbar_browser
            .as_ref()
            .is_some_and(|b| b.identifier() == id)
    }

    /// Браузер интерфейса: панель, всплывающее окно, разделитель DevTools
    /// (им доступен cefQuery).
    pub(crate) fn is_ui_browser(&self, browser: &Browser) -> bool {
        self.is_toolbar(browser) || self.is_popup(browser) || self.is_splitter(browser)
    }

    pub(crate) fn is_splitter(&self, browser: &Browser) -> bool {
        let id = browser.identifier();
        self.lock()
            .splitter_browser
            .as_ref()
            .is_some_and(|b| b.identifier() == id)
    }

    pub(crate) fn is_popup(&self, browser: &Browser) -> bool {
        let id = browser.identifier();
        self.lock()
            .popup_browser
            .as_ref()
            .is_some_and(|b| b.identifier() == id)
    }

    /// Роль браузера с учётом DevTools. DevTools в Chrome-стиле CEF создаёт
    /// с клиентом исследуемой вкладки, поэтому их события приходят с ролью
    /// `Tab(id)`. Браузер вкладки — тот, что записан в `Tab::browser`;
    /// любой другой с той же ролью — её DevTools.
    pub(crate) fn role_of(&self, role: Role, browser: Option<&Browser>) -> Role {
        let (Role::Tab(id), Some(browser)) = (role, browser) else {
            return role;
        };
        let s = self.lock();
        let own = s
            .tabs
            .iter()
            .chain(s.closing.iter())
            .find(|t| t.id == id)
            .and_then(|t| t.browser.as_ref().map(|b| b.identifier()));
        match own {
            Some(own) if own != browser.identifier() => Role::DevTools(id),
            _ => role,
        }
    }

    pub(crate) fn set_toolbar_browser(&self, browser: Browser) {
        self.lock().toolbar_browser = Some(browser);
    }

    pub(crate) fn set_tab_browser(&self, id: TabId, browser: Browser) {
        let s = &mut *self.lock();
        if let Some(tab) = s
            .tabs
            .iter_mut()
            .chain(s.closing.iter_mut())
            .find(|t| t.id == id)
        {
            tab.browser = Some(browser);
        }
    }

    /// Области панели, за которые можно таскать окно (координаты панели).
    pub(crate) fn set_draggable_regions(&self, mut regions: Vec<DraggableRegion>) {
        let (window, toolbar) = {
            let s = self.lock();
            (s.window.clone(), s.toolbar_view.clone())
        };
        let Some(window) = window else { return };
        // Окну нужны координаты окна; панель обычно стоит в (0, 0),
        // но пересчитываем честно.
        if let Some(toolbar) = toolbar {
            let mut origin = Point { x: 0, y: 0 };
            if View::from(&toolbar).convert_point_to_window(Some(&mut origin)) != 0 {
                for r in &mut regions {
                    r.bounds.x += origin.x;
                    r.bounds.y += origin.y;
                }
            }
        }
        window.set_draggable_regions(Some(&regions));
    }

    pub(crate) fn set_window_active(&self, active: bool) {
        self.lock().window_active = active;
        self.push_state();
    }

    pub(crate) fn set_fullscreen(&self, fullscreen: bool) {
        self.lock().fullscreen = fullscreen;
        self.push_state();
    }

    /// Изменить вкладку и, если она есть, обновить панель.
    pub(crate) fn update_tab(&self, id: TabId, f: impl FnOnce(&mut Tab)) {
        let found = {
            let mut s = self.lock();
            match s.tabs.iter_mut().find(|t| t.id == id) {
                Some(tab) => {
                    f(tab);
                    true
                }
                None => false,
            }
        };
        if found {
            self.push_state();
        }
    }

    pub(crate) fn browser_created(&self) {
        self.lock().live_browsers += 1;
    }

    pub(crate) fn browser_closed(&self, role: Role, browser: Option<Browser>) {

        if role.is_ui() {
            if let Some(router) = self.router() {
                router.on_before_close(browser);
            }
        }
        let (leftover, quit) = {
            let mut s = self.lock();
            s.live_browsers = s.live_browsers.saturating_sub(1);
            if role == Role::Toolbar {
                s.toolbar_browser = None;
                s.toolbar_ready = false;
            }
            if role == Role::Popup {
                s.popup_browser = None;
                s.popup_ready = false;
            }
            if role == Role::Splitter {
                s.splitter_browser = None;
                s.splitter_ready = false;
            }
            let leftover = match role {
                Role::Tab(id) => s
                    .closing
                    .iter()
                    .position(|t| t.id == id)
                    .map(|i| s.closing.remove(i)),
                _ => None,
            };
            (leftover, s.live_browsers == 0 && s.window_destroyed)
        };
        drop(leftover);
        if let Role::DevTools(id) = role {
            self.devtools_closed(id);
        }
        if quit {
            self.quit();
        }
    }

    /// `LifeSpanHandler::do_close`. 0 — пусть CEF закроет окно,
    /// 1 — закрытие обрабатываем сами (убираем только вкладку).
    pub(crate) fn do_close(&self, role: Role) -> c_int {
        if let Role::DevTools(id) = role {
            // Встроенные DevTools: убираем только их view (иначе CEF закрыл
            // бы всё окно). Отдельное окно DevTools CEF закроет сам.
            return self.devtools_do_close(id);
        }
        let Role::Tab(id) = role else {
            return 0;
        };
        enum Decision {
            CloseWindow,
            Detach(Option<TabId>),
        }
        let decision = {
            let mut s = self.lock();
            if s.window_closing {
                Decision::CloseWindow
            } else if s.closing.iter().any(|t| t.id == id) {
                Decision::Detach(None)
            } else if s.tabs.len() <= 1 && s.tab(id).is_some() {
                // Последняя вкладка закрылась сама (window.close()).
                Decision::CloseWindow
            } else {
                // Страница сама вызвала window.close().
                Decision::Detach(s.retire_tab(id).flatten())
            }
        };
        match decision {
            Decision::CloseWindow => 0,
            Decision::Detach(next) => {
                let shell = self.arc();
                post_ui(move || {
                    match next {
                        Some(next) => shell.activate(next),
                        None => shell.push_state(),
                    }
                    shell.detach_tab(id);
                });
                1
            }
        }
    }

    /// Ошибка загрузки главного фрейма: показываем свою страницу.
    pub(crate) fn show_load_error(
        &self,
        id: TabId,
        frame: &Frame,
        error: &str,
        failed_url: &str,
        cert_error: bool,
    ) {
        let heading = if cert_error {
            "Your connection isn't private"
        } else {
            "This page couldn't be opened"
        };
        let page = ui::error_page_url(heading, error, failed_url);
        {
            let mut s = self.lock();
            match s.tabs.iter_mut().find(|t| t.id == id) {
                Some(tab) => {
                    tab.error_page = Some((page.clone(), failed_url.to_string()));
                    tab.cert_error = cert_error;
                }
                None => return,
            }
        }
        frame.load_url(Some(&cef_str(&page)));
    }

    /// Renderer вкладки упал.
    pub(crate) fn show_crash(&self, id: TabId, browser: &Browser, details: &str) {
        let failed_url = self
            .lock()
            .tab(id)
            .map(|t| t.shown_url().to_string())
            .unwrap_or_default();
        if let Some(frame) = browser.main_frame() {
            let page = ui::error_page_url("This tab crashed", details, &failed_url);
            {
                let mut s = self.lock();
                if let Some(tab) = s.tabs.iter_mut().find(|t| t.id == id) {
                    tab.error_page = Some((page.clone(), failed_url.clone()));
                    tab.cert_error = false;
                }
            }
            frame.load_url(Some(&cef_str(&page)));
        }
    }
}

/// Масштаб в процентах по уровню Chromium (каждый шаг — ×1.2).
pub(crate) fn zoom_percent(level: f64) -> u32 {
    (100.0 * 1.2_f64.powf(level)).round() as u32
}

#[cfg(test)]
mod tests {
    use super::zoom_percent;

    #[test]
    fn zoom_levels() {
        assert_eq!(zoom_percent(0.0), 100);
        assert_eq!(zoom_percent(1.0), 120);
        assert_eq!(zoom_percent(-1.0), 83);
        assert_eq!(zoom_percent(1.2239010857415449), 125);
    }
}
