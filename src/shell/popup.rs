//! Всплывающее окно поверх страницы: главное меню и список загрузок.
//!
//! Панель высотой 80px не может рисовать за своими границами, поэтому
//! всплывающие окна живут в отдельном BrowserView (`ui/popup.html`),
//! добавленном в окно как overlay (CEF Views `add_overlay_view`).
//! Порядок показа:
//! 1. панель присылает `showPopup` с прямоугольником кнопки;
//! 2. мы запоминаем вид окна и отправляем всплывающему окну состояние;
//! 3. оно отрисовывается, меряет себя и присылает `popupSize`;
//! 4. мы ставим overlay под кнопку, показываем его и отдаём ему фокус.
//!
//! Закрывается окно по Esc, по потере фокуса и при любом другом действии.

use super::{cef_str, Shell, UI_MARKER};
use crate::client::{self, Role};
use crate::ipc::{Anchor, PopupKind};
use crate::{ui, views};
use cef::*;
use std::time::{Duration, Instant};

/// Поля вокруг карточки (как `--margin` в popup.html). Нулевые: браузер
/// в CEF Views не бывает прозрачным (при прозрачном фоне CEF рисует
/// CefSettings.background_color), поэтому слой совпадает с карточкой,
/// а тени нет.
const MARGIN: i32 = 0;
/// Отступ карточки от низа панели.
const GAP: i32 = 6;
/// Клик по кнопке, которая только что закрыла окно потерей фокуса,
/// не открывает его снова (иначе кнопка не умела бы закрывать окно).
const REOPEN_GUARD: Duration = Duration::from_millis(300);

impl Shell {
    /// Создать скрытое всплывающее окно. Вызывается при создании окна.
    pub(super) fn create_popup(&self, window: &Window) {
        let mut extra = dictionary_value_create();
        if let Some(extra) = extra.as_mut() {
            extra.set_bool(Some(&cef_str(UI_MARKER)), 1);
        }
        let mut client = client::new_client(self.arc(), Role::Popup);
        let mut delegate = views::PopupViewDelegate::new(self.arc());
        let Some(view) = browser_view_create(
            Some(&mut client),
            Some(&cef_str(ui::popup_url())),
            Some(&super::browser_settings(Role::Popup)),
            extra.as_mut(),
            None,
            Some(&mut delegate),
        ) else {
            eprintln!("rustium: не удалось создать всплывающее окно");
            return;
        };
        let overlay =
            window.add_overlay_view(Some(&mut View::from(&view)), DockingMode::CUSTOM, 1);
        if let Some(overlay) = &overlay {
            overlay.set_visible(0);
        }
        let mut s = self.lock();
        s.popup_view = Some(view);
        s.popup_overlay = overlay;
    }

    pub(crate) fn set_popup_browser(&self, browser: Browser) {
        self.lock().popup_browser = Some(browser);
    }

    pub(super) fn show_popup(&self, kind: PopupKind, anchor: Anchor) {
        let toggle_off = {
            let mut s = self.lock();
            let just_closed = s
                .popup_closed
                .is_some_and(|(k, at)| k == kind && at.elapsed() < REOPEN_GUARD);
            if s.popup == Some(kind) || just_closed {
                true
            } else {
                s.popup = Some(kind);
                s.popup_anchor = Some(anchor);
                if kind == PopupKind::Downloads {
                    s.downloads_seen = true;
                }
                false
            }
        };
        if toggle_off {
            self.close_popup(false);
            return;
        }
        // Окно ещё скрыто: если оно уже было открыто другим видом, прячем.
        let overlay = self.lock().popup_overlay.clone();
        if let Some(overlay) = overlay {
            overlay.set_visible(0);
        }
        if kind == PopupKind::Menu {
            // refresh_zoom сам отправит состояние.
            self.refresh_zoom();
        }
        self.push_state();
    }

    /// Всплывающее окно отрисовалось: ставим его под кнопку и показываем.
    pub(super) fn popup_size(&self, width: f64, height: f64) {
        let (kind, anchor, overlay, view, window) = {
            let s = self.lock();
            let Some(kind) = s.popup else {
                return;
            };
            (
                kind,
                s.popup_anchor,
                s.popup_overlay.clone(),
                s.popup_view.clone(),
                s.window.clone(),
            )
        };
        let (Some(anchor), Some(overlay), Some(view)) = (anchor, overlay, view) else {
            return;
        };
        let window_size = window.map(|w| w.bounds());
        let bounds = popup_bounds(anchor, width, height, window_size.as_ref());
        overlay.set_bounds(Some(&bounds));
        overlay.set_visible(1);
        #[cfg(not(target_os = "macos"))]
        let _ = kind;
        #[cfg(target_os = "macos")]
        {
            let radius = match kind {
                PopupKind::Menu => 14.0,
                PopupKind::Downloads => 16.0,
            };
            let rounded = crate::mac::style_popup_window(
                bounds.width as f64,
                bounds.height as f64,
                radius,
            );
            if std::env::var_os("RUSTIUM_DEBUG_POPUP").is_some() {
                eprintln!("[popup] {kind:?} {}x{} скруглено: {rounded}", bounds.width, bounds.height);
            }
        }
        view.request_focus();
    }

    /// Закрыть всплывающее окно. `by_user` — его закрыли из самого окна
    /// (Esc, потеря фокуса), а не другим действием.
    pub(super) fn close_popup(&self, by_user: bool) {
        let overlay = {
            let mut s = self.lock();
            let Some(kind) = s.popup.take() else {
                return;
            };
            if by_user {
                s.popup_closed = Some((kind, Instant::now()));
            }
            s.popup_overlay.clone()
        };
        if let Some(overlay) = overlay {
            overlay.set_visible(0);
        }
        self.push_state();
    }

    pub(super) fn run_in_popup(&self, code: &str) {
        let browser = self.lock().popup_browser.clone();
        if let Some(frame) = browser.and_then(|b| b.main_frame()) {
            frame.execute_java_script(Some(&cef_str(code)), Some(&cef_str("rustium://popup")), 0);
        }
    }
}

/// Где поставить всплывающее окно размером `width`×`height` (с полями
/// под тень): правым краем карточки под правый край кнопки, сразу под
/// панелью, не выходя за окно.
fn popup_bounds(anchor: Anchor, width: f64, height: f64, window: Option<&Rect>) -> Rect {
    let width = width.ceil() as i32;
    let mut height = height.ceil() as i32;
    let right = (anchor.x + anchor.width).round() as i32;
    let mut x = right + MARGIN - width;
    let y = ui::TOOLBAR_HEIGHT + GAP - MARGIN;
    // Карточка не выходит за окно (поля, если они есть, могут).
    if let Some(w) = window {
        x = x.min(w.width - width + MARGIN).max(-MARGIN);
        height = height.min(w.height - y).max(0);
    } else {
        x = x.max(-MARGIN);
    }
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(x: f64) -> Anchor {
        Anchor {
            x,
            y: 44.0,
            width: 30.0,
            height: 30.0,
        }
    }

    #[test]
    fn popup_sits_under_the_button() {
        let window = Rect { x: 0, y: 0, width: 1280, height: 840 };
        // Кнопка меню у правого края: карточка шириной 300 + поля.
        let r = popup_bounds(anchor(1242.0), 300.0 + 2.0 * MARGIN as f64, 500.0, Some(&window));
        assert_eq!(r.x + r.width - MARGIN, 1272);
        assert_eq!(r.y + MARGIN, 86);
        // Слишком высокое окно обрезается по низу окна.
        let r = popup_bounds(anchor(1242.0), 300.0 + 2.0 * MARGIN as f64, 2000.0, Some(&window));
        assert_eq!(r.y + r.height, 840);
        // Кнопка у левого края: карточка не уходит за левую границу.
        let r = popup_bounds(anchor(10.0), 432.0, 100.0, Some(&window));
        assert_eq!(r.x + MARGIN, 0);
    }
}
