//! DevTools отдельным окном — штатные DevTools CEF (`host.show_dev_tools`).
//!
//! Встроить штатные DevTools в окно рядом со страницей в CEF 156 нельзя:
//! CEF патчем к Chrome запрещает встраивание для своих браузеров
//! (подробно — docs/devtools-research.md в ветке devtools-native).
//! В ветке main есть вариант со встроенными DevTools через свой фронтенд.
//!
//! Панель вкладок не использует layout-менеджер: размеры страниц задаёт
//! `layout_content`.

use super::{Shell, TabId};
use crate::client::{self, Role};
use cef::*;
use std::os::raw::c_int;

impl Shell {
    /// F12, ⌥⌘I, пункт меню: открыть или закрыть DevTools активной вкладки.
    pub(super) fn toggle_devtools(&self) {
        let (id, browser) = {
            let s = self.lock();
            let Some(tab) = s.active_tab() else { return };
            (tab.id, tab.browser.clone())
        };
        let Some(host) = browser.and_then(|b| b.host()) else { return };
        if host.has_dev_tools() != 0 {
            host.close_dev_tools();
            self.lock().devtools_windows.remove(&id);
        } else {
            self.lock().devtools_windows.insert(id);
            // Свой клиент нужен, чтобы узнать о закрытии окна DevTools
            // (on_before_close с ролью DevTools).
            let mut client = client::new_client(self.arc(), Role::DevTools(id));
            host.show_dev_tools(
                Some(&WindowInfo::default()),
                Some(&mut client),
                Some(&BrowserSettings::default()),
                None,
            );
        }
        self.push_state();
    }

    /// `do_close` для роли DevTools: окно DevTools CEF закрывает само.
    pub(super) fn devtools_do_close(&self, _id: TabId) -> c_int {
        0
    }

    /// Браузер DevTools закрылся (`on_before_close`).
    pub(super) fn devtools_closed(&self, id: TabId) {
        self.lock().devtools_windows.remove(&id);
        self.push_state();
    }

    /// Разложить страницы по панели вкладок: каждая на всю панель.
    /// Вызывается при изменении размера панели и при переключении вкладок.
    pub(crate) fn layout_content(&self) {
        let (content, views) = {
            let s = self.lock();
            let views: Vec<BrowserView> = s.tabs.iter().map(|t| t.view.clone()).collect();
            (s.content.clone(), views)
        };
        let Some(content) = content else { return };
        let bounds = content.bounds();
        let full = Rect { x: 0, y: 0, width: bounds.width, height: bounds.height };
        for view in &views {
            view.set_bounds(Some(&full));
        }
    }
}

impl super::State {
    /// У вкладки открыто окно DevTools.
    pub(super) fn devtools_open(&self, id: TabId) -> bool {
        self.devtools_windows.contains(&id)
    }
}
