//! DevTools внутри окна (справа или снизу от страницы) или отдельным окном.
//!
//! Как это работает:
//! - Отдельное окно — штатные DevTools CEF (`host.show_dev_tools`).
//! - Встроенные — свой Alloy-BrowserView с фронтендом DevTools
//!   (`devtools://devtools/bundled/devtools_app.html?ws=…`), который через
//!   WebSocket-мост (devtools_bridge.rs) говорит со вкладкой по CDP.
//!   Штатные DevTools CEF встроить нельзя: они бывают только в Chrome-стиле,
//!   а окно Chrome-стиля ломается, когда такой браузер закрывается.
//! - Панель вкладок не использует layout-менеджер: размеры страниц, DevTools
//!   и разделителя считает `layout_content` (функция `split`).
//! - Разделитель — тонкий BrowserView (`ui/splitter.html`): тянут его мышью,
//!   он присылает `splitterMove`, мы меняем размер DevTools.
//! - Закрытие встроенных: `close_browser` их браузера → `do_close` для роли
//!   DevTools возвращает 1 и убирает view (`detach_devtools`) → CEF
//!   уничтожает браузер → `on_before_close` (`devtools_closed`).

use super::{cef_str, Shell, TabId, UI_MARKER};
use crate::client::{self, Role};
use crate::devtools_bridge::{self, Bridge};
use crate::ipc::Dock;
use crate::{ui, views};
use cef::*;
use std::os::raw::c_int;
use std::sync::Arc;

/// Толщина разделителя между страницей и DevTools.
pub(super) const SPLITTER: i32 = 4;
/// Размер DevTools по умолчанию: ширина справа, высота снизу.
pub(super) const DEFAULT_WIDTH: i32 = 480;
pub(super) const DEFAULT_HEIGHT: i32 = 320;
/// Ни DevTools, ни страница не становятся меньше этого.
const MIN_DEVTOOLS: i32 = 200;
const MIN_PAGE: i32 = 240;

/// Прямоугольники страницы, разделителя и DevTools внутри панели
/// размером `width`×`height`. `size` — желаемая ширина (справа) или
/// высота (снизу) DevTools; итоговая возвращается четвёртым значением.
pub(super) fn split(width: i32, height: i32, dock: Dock, size: i32) -> (Rect, Rect, Rect, i32) {
    let rect = |x, y, width, height| Rect { x, y, width, height };
    let total = if dock == Dock::Bottom { height } else { width };
    let max = (total - MIN_PAGE - SPLITTER).max(MIN_DEVTOOLS.min(total / 2));
    let size = size.clamp(MIN_DEVTOOLS.min(max), max.max(0));
    match dock {
        Dock::Bottom => {
            let page_h = (height - size - SPLITTER).max(0);
            (
                rect(0, 0, width, page_h),
                rect(0, page_h, width, SPLITTER),
                rect(0, page_h + SPLITTER, width, size),
                size,
            )
        }
        // Window сюда не попадает (страница на всю панель), но и для него
        // разумно вернуть раскладку «справа».
        Dock::Right | Dock::Window => {
            let page_w = (width - size - SPLITTER).max(0);
            (
                rect(0, 0, page_w, height),
                rect(page_w, 0, SPLITTER, height),
                rect(page_w + SPLITTER, 0, size, height),
                size,
            )
        }
    }
}

impl Shell {
    /// Создать разделитель (скрытый). Вызывается при создании окна.
    pub(super) fn create_splitter(&self, content: &Panel) {
        let mut extra = dictionary_value_create();
        if let Some(extra) = extra.as_mut() {
            extra.set_bool(Some(&cef_str(UI_MARKER)), 1);
        }
        let mut client = client::new_client(self.arc(), Role::Splitter);
        let mut delegate = views::SplitterViewDelegate::new(self.arc());
        let Some(view) = browser_view_create(
            Some(&mut client),
            Some(&cef_str(ui::splitter_url())),
            Some(&super::browser_settings(Role::Splitter)),
            extra.as_mut(),
            None,
            Some(&mut delegate),
        ) else {
            return;
        };
        view.set_visible(0);
        content.add_child_view(Some(&mut View::from(&view)));
        self.lock().splitter_view = Some(view);
    }

    pub(crate) fn set_splitter_browser(&self, browser: Browser) {
        self.lock().splitter_browser = Some(browser);
    }

    /// F12, ⌥⌘I, пункт меню: открыть или закрыть DevTools активной вкладки.
    pub(super) fn toggle_devtools(&self) {
        let (id, browser) = {
            let s = self.lock();
            let Some(tab) = s.active_tab() else { return };
            (tab.id, tab.browser.clone())
        };
        let Some(host) = browser.and_then(|b| b.host()) else { return };
        if self.devtools_really_open(id, &host) {
            self.close_devtools(id, &host);
        } else {
            self.open_devtools(id, &host);
        }
    }

    /// Открыты ли DevTools вкладки. Отдельное окно сверяем с CEF: если
    /// его закрыли, а событие не пришло, забываем его.
    fn devtools_really_open(&self, id: TabId, host: &BrowserHost) -> bool {
        let native = host.has_dev_tools() != 0;
        let mut s = self.lock();
        if !native {
            s.devtools_windows.remove(&id);
        }
        s.devtools_views.contains_key(&id) || native
    }

    fn open_devtools(&self, id: TabId, host: &BrowserHost) {
        let dock = self.lock().dock;
        if dock == Dock::Window {
            // Отдельное окно — штатные DevTools CEF. Свой клиент нужен,
            // чтобы узнать о закрытии окна (on_before_close с ролью DevTools).
            self.lock().devtools_windows.insert(id);
            let mut client = client::new_client(self.arc(), Role::DevTools(id));
            host.show_dev_tools(
                Some(&WindowInfo::default()),
                Some(&mut client),
                Some(&BrowserSettings::default()),
                None,
            );
            self.push_state();
        } else {
            self.open_docked_devtools(id, host);
        }
    }

    /// Встроенные DevTools: свой BrowserView с фронтендом DevTools,
    /// связанный со вкладкой через мост (devtools_bridge.rs).
    fn open_docked_devtools(&self, id: TabId, host: &BrowserHost) {
        let weak = self.weak_self.clone();
        let Some(bridge) = devtools_bridge::bridge(|| {
            Arc::new(move |tab, message: String| {
                if let Some(shell) = weak.upgrade() {
                    super::post_ui(move || shell.devtools_from_frontend(tab, message));
                }
            })
        }) else {
            return;
        };
        let content = {
            let s = self.lock();
            if s.window_closing || s.window_destroyed || s.devtools_views.contains_key(&id) {
                return;
            }
            s.content.clone()
        };
        let Some(content) = content else { return };

        // Сообщения вкладки → фронтенд.
        let mut observer = DevToolsObserver::new(bridge.clone(), id);
        let registration = host.add_dev_tools_message_observer(Some(&mut observer));

        let url = format!(
            "devtools://devtools/bundled/devtools_app.html?ws={}",
            bridge.ws_address(id)
        );
        let mut client = client::new_client(self.arc(), Role::DevTools(id));
        let mut delegate = views::DevToolsViewDelegate::new(self.arc(), id);
        let Some(view) = browser_view_create(
            Some(&mut client),
            Some(&cef_str(&url)),
            Some(&super::browser_settings(Role::DevTools(id))),
            None,
            None,
            Some(&mut delegate),
        ) else {
            return;
        };
        {
            let mut s = self.lock();
            s.devtools_views.insert(id, view.clone());
            if let Some(registration) = registration {
                s.devtools_registrations.insert(id, registration);
            }
        }
        content.add_child_view(Some(&mut View::from(&view)));
        self.layout_content();
        self.push_state();
    }

    /// Закрыть DevTools вкладки (встроенные или отдельным окном).
    pub(super) fn close_devtools(&self, id: TabId, host: &BrowserHost) {
        let docked = {
            let s = self.lock();
            s.devtools_views.contains_key(&id)
                .then(|| s.devtools_browsers.get(&id).cloned())
        };
        match docked {
            Some(browser) => {
                self.stop_devtools_bridge(id);
                match browser.and_then(|b| b.host()) {
                    // Дальше do_close (роль DevTools) уберёт view.
                    Some(devtools_host) => devtools_host.close_browser(1),
                    None => self.detach_devtools(id),
                }
            }
            None => host.close_dev_tools(),
        }
    }

    /// Отключить вкладку от фронтенда: снять observer и закрыть сеанс.
    fn stop_devtools_bridge(&self, id: TabId) {
        let registration = self.lock().devtools_registrations.remove(&id);
        drop(registration);
        if let Some(bridge) = devtools_bridge::bridge(|| Arc::new(|_, _| {})) {
            bridge.close(id);
        }
    }

    /// Сообщение CDP от фронтенда встроенных DevTools → вкладке.
    fn devtools_from_frontend(&self, id: TabId, message: String) {
        let browser = self.lock().tab(id).and_then(|t| t.browser.clone());
        if let Some(host) = browser.and_then(|b| b.host()) {
            host.send_dev_tools_message(Some(message.as_bytes()));
        }
    }

    pub(crate) fn set_devtools_browser(&self, id: TabId, browser: Browser) {
        self.lock().devtools_browsers.insert(id, browser);
    }

    /// `do_close` для роли DevTools. 1 — встроенные: убираем только view.
    pub(super) fn devtools_do_close(&self, id: TabId) -> c_int {
        let docked = {
            let s = self.lock();
            !s.window_closing && s.devtools_views.contains_key(&id)
        };
        if !docked {
            return 0;
        }
        let shell = self.arc();
        super::post_ui(move || shell.detach_devtools(id));
        1
    }

    /// Встроенные DevTools закрылись: убираем их view из окна.
    pub(super) fn detach_devtools(&self, id: TabId) {
        self.stop_devtools_bridge(id);
        let (content, view) = {
            let mut s = self.lock();
            (s.content.clone(), s.devtools_views.remove(&id))
        };
        let Some(view) = view else { return };
        if let Some(content) = content {
            content.remove_child_view(Some(&mut View::from(&view)));
        }
        drop(view);
        self.layout_content();
        self.push_state();
        self.focus_content();
    }

    /// Встроенные DevTools вкладки, которую закрывают: закрыть и их.
    pub(super) fn close_devtools_of(&self, id: TabId) {
        let browser = self.lock().devtools_browsers.get(&id).cloned();
        if let Some(host) = browser.and_then(|b| b.host()) {
            self.stop_devtools_bridge(id);
            host.close_browser(1);
        }
    }

    /// Браузер DevTools закрылся (`on_before_close`).
    pub(super) fn devtools_closed(&self, id: TabId) {
        let (reopen, leftover) = {
            let mut s = self.lock();
            s.devtools_windows.remove(&id);
            let leftover = s.devtools_browsers.remove(&id);
            let reopen = s.devtools_reopen == Some(id);
            if reopen {
                s.devtools_reopen = None;
            }
            (reopen, leftover)
        };
        drop(leftover);
        if reopen {
            // Смена «в окне» ↔ «отдельным окном»: открываем заново.
            let shell = self.arc();
            super::post_ui(move || shell.reopen_devtools(id));
        } else {
            self.push_state();
        }
    }

    fn reopen_devtools(&self, id: TabId) {
        let browser = self.lock().tab(id).and_then(|t| t.browser.clone());
        if let Some(host) = browser.and_then(|b| b.host()) {
            self.open_devtools(id, &host);
        }
    }

    pub(super) fn set_devtools_dock(&self, dock: Dock) {
        let (old, active, browser) = {
            let mut s = self.lock();
            let old = std::mem::replace(&mut s.dock, dock);
            (old, s.active, s.active_tab().and_then(|t| t.browser.clone()))
        };
        let docked = |d: Dock| d != Dock::Window;
        if let (Some(id), Some(host)) = (active, browser.and_then(|b| b.host())) {
            if docked(old) != docked(dock) && self.devtools_really_open(id, &host) {
                if docked(old) {
                    // Встроенные → окно: откроем, когда наш браузер DevTools
                    // закроется (devtools_closed).
                    self.lock().devtools_reopen = Some(id);
                    self.close_devtools(id, &host);
                } else {
                    // Окно → встроенные: штатное окно закрываем, встроенные
                    // открываем сразу (они от него не зависят).
                    host.close_dev_tools();
                    self.lock().devtools_windows.remove(&id);
                    self.open_devtools(id, &host);
                }
            }
        }
        self.layout_content();
        self.push_state();
    }

    /// Разделитель тянут мышью.
    pub(super) fn splitter_move(&self, dx: f64, dy: f64) {
        {
            let mut s = self.lock();
            match s.dock {
                Dock::Right => s.devtools_width -= dx.round() as i32,
                Dock::Bottom => s.devtools_height -= dy.round() as i32,
                Dock::Window => return,
            }
        }
        self.layout_content();
    }

    /// Разложить страницы, DevTools и разделитель по панели вкладок.
    /// Вызывается при изменении размера панели и после любых перестановок.
    pub(crate) fn layout_content(&self) {
        struct Item {
            view: BrowserView,
            devtools: Option<BrowserView>,
            active: bool,
        }
        let (content, items, splitter, dock, size) = {
            let s = self.lock();
            let items: Vec<Item> = s
                .tabs
                .iter()
                .map(|t| Item {
                    view: t.view.clone(),
                    devtools: s.devtools_views.get(&t.id).cloned(),
                    active: s.active == Some(t.id),
                })
                .collect();
            let size = if s.dock == Dock::Bottom { s.devtools_height } else { s.devtools_width };
            (s.content.clone(), items, s.splitter_view.clone(), s.dock, size)
        };
        let Some(content) = content else { return };
        let bounds = content.bounds();
        let full = Rect { x: 0, y: 0, width: bounds.width, height: bounds.height };
        let (page, line, tools, used) = split(bounds.width, bounds.height, dock, size);

        let mut splitter_shown = false;
        for item in &items {
            match &item.devtools {
                Some(devtools) if dock != Dock::Window => {
                    item.view.set_bounds(Some(&page));
                    devtools.set_bounds(Some(&tools));
                    devtools.set_visible(item.active as c_int);
                    if item.active {
                        if let Some(splitter) = &splitter {
                            splitter.set_bounds(Some(&line));
                            splitter.set_visible(1);
                            splitter_shown = true;
                        }
                    }
                }
                Some(devtools) => {
                    devtools.set_visible(0);
                    item.view.set_bounds(Some(&full));
                }
                None => item.view.set_bounds(Some(&full)),
            }
        }
        if !splitter_shown {
            if let Some(splitter) = &splitter {
                splitter.set_visible(0);
            }
        }
        // Запоминаем размер после ограничений, чтобы разделитель
        // не «уезжал» за пределы при перетаскивании.
        if splitter_shown {
            let mut s = self.lock();
            match dock {
                Dock::Bottom => s.devtools_height = used,
                _ => s.devtools_width = used,
            }
        }
    }

    pub(super) fn run_in_splitter(&self, code: &str) {
        let browser = self.lock().splitter_browser.clone();
        if let Some(frame) = browser.and_then(|b| b.main_frame()) {
            frame.execute_java_script(Some(&cef_str(code)), Some(&cef_str("rustium://splitter")), 0);
        }
    }
}

impl super::State {
    /// У вкладки открыты DevTools (встроенные или отдельным окном).
    pub(super) fn devtools_open(&self, id: TabId) -> bool {
        self.devtools_views.contains_key(&id) || self.devtools_windows.contains(&id)
    }
}

wrap_dev_tools_message_observer! {
    // Ответы и события CDP вкладки → её фронтенду DevTools.
    struct DevToolsObserver {
        bridge: Arc<Bridge>,
        tab: TabId,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_message(
            &self,
            _browser: Option<&mut Browser>,
            message: Option<&[u8]>,
        ) -> ::std::os::raw::c_int {
            if let Some(message) = message {
                self.bridge.send(self.tab, String::from_utf8_lossy(message).into_owned());
            }
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_right_and_bottom() {
        let (page, line, tools, used) = split(1280, 760, Dock::Right, 480);
        assert_eq!(used, 480);
        assert_eq!(page.width + line.width + tools.width, 1280);
        assert_eq!(tools.x, 1280 - 480);
        assert_eq!(page.height, 760);

        let (page, line, tools, _) = split(1280, 760, Dock::Bottom, 320);
        assert_eq!(page.height + line.height + tools.height, 760);
        assert_eq!(tools.y, 760 - 320);
        assert_eq!(tools.width, 1280);
    }

    #[test]
    fn split_keeps_minimums() {
        // DevTools не уже минимума.
        assert_eq!(split(1280, 760, Dock::Right, 10).3, MIN_DEVTOOLS);
        // Страница не уже минимума.
        let (page, ..) = split(1280, 760, Dock::Right, 5000);
        assert_eq!(page.width, MIN_PAGE);
        // В крошечном окне ничего не уходит в минус.
        let (page, line, tools, _) = split(300, 200, Dock::Right, 480);
        assert!(page.width >= 0 && tools.width >= 0);
        assert!(page.width + line.width + tools.width <= 300);
    }
}
