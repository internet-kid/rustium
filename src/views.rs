//! Делегаты CEF Views: окно, панель и вкладки.
//!
//! Используется Alloy-стиль: он позволяет держать несколько BrowserView
//! в одном окне и не добавляет интерфейс Chrome. Стиль окна и браузеров
//! должен совпадать.

use crate::shell::{Shell, TabId};
use crate::ui::TOOLBAR_HEIGHT;
use cef::*;
use std::sync::Arc;

const MIN_WIDTH: i32 = 480;
const MIN_HEIGHT: i32 = 320;

/// macOS: окно без системного заголовка, «светофор» стоит прямо
/// в полосе вкладок (отступ под него — класс `frameless` в toolbar.html).
/// На Linux рамка обычная.
pub const FRAMELESS: bool = cfg!(target_os = "macos");

/// Высота «заголовка», по которой macOS центрирует кнопки окна.
/// Вкладки занимают нижние 32px полосы высотой 38px, их центр — на 22px,
/// поэтому заголовок 44px ставит кнопки вровень с вкладками.
const TITLEBAR_HEIGHT: f32 = 44.0;

wrap_window_delegate! {
    pub struct RustiumWindowDelegate {
        shell: Arc<Shell>,
    }

    impl ViewDelegate {
        fn minimum_size(&self, _view: Option<&mut View>) -> Size {
            Size { width: MIN_WIDTH, height: MIN_HEIGHT }
        }
    }

    impl PanelDelegate {}

    impl WindowDelegate {
        fn on_window_created(&self, window: Option<&mut Window>) {
            if let Some(window) = window {
                self.shell.on_window_created(window.clone());
            }
        }

        fn on_window_destroyed(&self, _window: Option<&mut Window>) {
            self.shell.on_window_destroyed();
        }

        fn can_close(&self, _window: Option<&mut Window>) -> ::std::os::raw::c_int {
            self.shell.can_close_window() as ::std::os::raw::c_int
        }

        fn window_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }

        // В крейте `cef` эти методы по умолчанию возвращают 0 (в C++ CEF — true),
        // из-за этого у окна без рамки была неактивна зелёная кнопка.
        fn can_resize(&self, _window: Option<&mut Window>) -> ::std::os::raw::c_int {
            1
        }

        fn can_maximize(&self, _window: Option<&mut Window>) -> ::std::os::raw::c_int {
            1
        }

        fn can_minimize(&self, _window: Option<&mut Window>) -> ::std::os::raw::c_int {
            1
        }

        fn is_frameless(&self, _window: Option<&mut Window>) -> ::std::os::raw::c_int {
            FRAMELESS as ::std::os::raw::c_int
        }

        fn with_standard_window_buttons(
            &self,
            _window: Option<&mut Window>,
        ) -> ::std::os::raw::c_int {
            FRAMELESS as ::std::os::raw::c_int
        }

        fn titlebar_height(
            &self,
            _window: Option<&mut Window>,
            titlebar_height: Option<&mut f32>,
        ) -> ::std::os::raw::c_int {
            match titlebar_height {
                Some(height) if FRAMELESS => {
                    *height = TITLEBAR_HEIGHT;
                    1
                }
                _ => 0,
            }
        }

        // Неактивное окно панель рисует приглушённо.
        fn on_window_activation_changed(
            &self,
            _window: Option<&mut Window>,
            active: ::std::os::raw::c_int,
        ) {
            self.shell.set_window_active(active != 0);
        }

        // В полноэкранном режиме кнопок окна нет — панель убирает отступ.
        fn on_window_fullscreen_transition(
            &self,
            window: Option<&mut Window>,
            is_completed: ::std::os::raw::c_int,
        ) {
            if is_completed != 0 {
                let fullscreen = window.is_some_and(|w| w.is_fullscreen() != 0);
                self.shell.set_fullscreen(fullscreen);
            }
        }
    }
}

wrap_browser_view_delegate! {
    pub struct ToolbarViewDelegate {
        shell: Arc<Shell>,
    }

    impl ViewDelegate {
        // Панель фиксированной высоты; ширину задаёт BoxLayout (STRETCH).
        // Ширина не 0: размер с нулевой стороной CEF считает «пустым»
        // и тогда игнорирует ответ делегата (панель растягивается).
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            Size { width: 1, height: TOOLBAR_HEIGHT }
        }

        fn minimum_size(&self, _view: Option<&mut View>) -> Size {
            Size { width: 1, height: TOOLBAR_HEIGHT }
        }

        fn height_for_width(
            &self,
            _view: Option<&mut View>,
            _width: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            TOOLBAR_HEIGHT
        }
    }

    impl BrowserViewDelegate {
        fn on_browser_created(
            &self,
            _browser_view: Option<&mut BrowserView>,
            browser: Option<&mut Browser>,
        ) {
            if let Some(browser) = browser {
                self.shell.set_toolbar_browser(browser.clone());
            }
        }

        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

wrap_browser_view_delegate! {
    pub struct TabViewDelegate {
        shell: Arc<Shell>,
        tab: TabId,
    }

    impl ViewDelegate {}

    impl BrowserViewDelegate {
        fn on_browser_created(
            &self,
            _browser_view: Option<&mut BrowserView>,
            browser: Option<&mut Browser>,
        ) {
            if let Some(browser) = browser {
                self.shell.set_tab_browser(self.tab, browser.clone());
            }
        }

        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

wrap_panel_delegate! {
    pub struct ContentPanelDelegate {
        shell: Arc<Shell>,
    }

    impl ViewDelegate {
        // Панель вкладок без layout-менеджера: при каждом изменении
        // размера раскладываем страницы и DevTools сами.
        fn on_layout_changed(&self, _view: Option<&mut View>, _new_bounds: Option<&Rect>) {
            self.shell.layout_content();
        }
    }

    impl PanelDelegate {}
}

wrap_browser_view_delegate! {
    pub struct PopupViewDelegate {
        shell: Arc<Shell>,
    }

    impl ViewDelegate {}

    impl BrowserViewDelegate {
        fn on_browser_created(
            &self,
            _browser_view: Option<&mut BrowserView>,
            browser: Option<&mut Browser>,
        ) {
            if let Some(browser) = browser {
                self.shell.set_popup_browser(browser.clone());
            }
        }

        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}
