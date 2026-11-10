//! Горячие клавиши. Срабатывают и в панели, и на страницах
//! (вызываются из `KeyboardHandler::on_pre_key_event`).
//!
//! «Главный» модификатор: Cmd на macOS, Ctrl на Linux/Windows.

use crate::shell::{Action, Shell};
use crate::task::post_ui;
use cef::*;
use std::sync::Arc;

// cef_event_flags_t
const SHIFT: u32 = 1 << 1;
const CONTROL: u32 = 1 << 2;
const ALT: u32 = 1 << 3;
const COMMAND: u32 = 1 << 7;

// Windows virtual-key codes (CEF использует их на всех платформах).
mod vk {
    pub const TAB: i32 = 0x09;
    /// VK_OEM_COMMA — «,».
    pub const COMMA: i32 = 0xBC;
    pub const LEFT: i32 = 0x25;
    pub const RIGHT: i32 = 0x27;
    pub const KEY_0: i32 = 0x30;
    pub const KEY_1: i32 = 0x31;
    pub const KEY_8: i32 = 0x38;
    pub const KEY_9: i32 = 0x39;
    pub const A: i32 = 0x41;
    pub const C: i32 = 0x43;
    pub const I: i32 = 0x49;
    pub const L: i32 = 0x4C;
    pub const P: i32 = 0x50;
    pub const Q: i32 = 0x51;
    pub const R: i32 = 0x52;
    pub const T: i32 = 0x54;
    pub const V: i32 = 0x56;
    pub const W: i32 = 0x57;
    pub const X: i32 = 0x58;
    pub const Z: i32 = 0x5A;
    pub const NUMPAD_0: i32 = 0x60;
    pub const ADD: i32 = 0x6B;
    pub const SUBTRACT: i32 = 0x6D;
    pub const F5: i32 = 0x74;
    pub const F12: i32 = 0x7B;
    pub const PLUS: i32 = 0xBB; // "=" / "+"
    pub const MINUS: i32 = 0xBD;
    pub const BRACKET_LEFT: i32 = 0xDB;
    pub const BRACKET_RIGHT: i32 = 0xDD;
}

#[derive(Clone, Copy)]
struct Keys {
    code: i32,
    primary: bool,
    ctrl: bool,
    shift: bool,
    alt: bool,
}

/// Что делать с сочетанием клавиш.
enum Outcome {
    Action(Action),
    Edit(EditCommand),
}

#[derive(Clone, Copy)]
enum EditCommand {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// Возвращает `true`, если сочетание обработано (дальше его не передаём).
pub fn handle(shell: &Arc<Shell>, browser: Option<&mut Browser>, event: Option<&KeyEvent>) -> bool {
    let Some(event) = event else { return false };
    if event.type_.get_raw() != KeyEventType::RAWKEYDOWN.get_raw() {
        return false;
    }
    let m = event.modifiers;
    let keys = Keys {
        code: event.windows_key_code,
        primary: if cfg!(target_os = "macos") {
            m & COMMAND != 0
        } else {
            m & CONTROL != 0
        },
        ctrl: m & CONTROL != 0,
        shift: m & SHIFT != 0,
        alt: m & ALT != 0,
    };
    match outcome(keys) {
        None => false,
        Some(Outcome::Action(action)) => {
            let shell = shell.clone();
            post_ui(move || shell.perform(action));
            true
        }
        Some(Outcome::Edit(cmd)) => {
            // Правка выполняется в том браузере, где нажали клавиши
            // (это может быть и адресная строка панели).
            if let Some(frame) = browser.and_then(|b| b.focused_frame()) {
                match cmd {
                    EditCommand::Undo => frame.undo(),
                    EditCommand::Redo => frame.redo(),
                    EditCommand::Cut => frame.cut(),
                    EditCommand::Copy => frame.copy(),
                    EditCommand::Paste => frame.paste(),
                    EditCommand::SelectAll => frame.select_all(),
                }
            }
            true
        }
    }
}

fn outcome(k: Keys) -> Option<Outcome> {
    use Action::*;
    let mac = cfg!(target_os = "macos");
    let plain = !k.shift && !k.alt;

    let action = match k.code {
        vk::T if k.primary && plain => NewTab,
        vk::W if k.primary && plain => CloseActiveTab,
        vk::L if k.primary && plain => FocusAddress,
        vk::R if k.primary && plain => Reload,
        vk::F5 if !k.primary && plain => Reload,
        // Ctrl+Tab работает и на macOS, как в Chrome.
        vk::TAB if k.ctrl && !k.alt => {
            if k.shift {
                PrevTab
            } else {
                NextTab
            }
        }
        vk::BRACKET_RIGHT if mac && k.primary && k.shift => NextTab,
        vk::BRACKET_LEFT if mac && k.primary && k.shift => PrevTab,
        vk::KEY_1..=vk::KEY_8 if k.primary && plain => SelectIndex((k.code - vk::KEY_1) as usize),
        vk::KEY_9 if k.primary && plain => SelectLast,
        // Назад/вперёд: Alt+←/→ (Linux), Cmd+[ / Cmd+] (macOS).
        vk::LEFT if !mac && k.alt && !k.primary && !k.shift => Back,
        vk::RIGHT if !mac && k.alt && !k.primary && !k.shift => Forward,
        vk::BRACKET_LEFT if mac && k.primary && plain => Back,
        vk::BRACKET_RIGHT if mac && k.primary && plain => Forward,
        // DevTools: F12, Ctrl+Shift+I (Linux), Cmd+Alt+I (macOS).
        vk::F12 => DevTools,
        vk::I if !mac && k.primary && k.shift && !k.alt => DevTools,
        vk::I if mac && k.primary && k.alt && !k.shift => DevTools,
        vk::PLUS | vk::ADD if k.primary && !k.alt => ZoomIn,
        vk::MINUS | vk::SUBTRACT if k.primary && !k.alt => ZoomOut,
        vk::KEY_0 | vk::NUMPAD_0 if k.primary && plain => ZoomReset,
        vk::Q if mac && k.primary && plain => Quit,
        vk::P if k.primary && plain => Print,
        // Настройки: Cmd+, (macOS).
        vk::COMMA if mac && k.primary && plain => OpenSettings,
        _ => return edit_outcome(k, mac).map(Outcome::Edit),
    };
    Some(Outcome::Action(action))
}

/// На macOS без меню «Правка» Cmd+C/V/X/A/Z сами не работают,
/// поэтому выполняем их вручную. На Linux это делает Chromium.
fn edit_outcome(k: Keys, mac: bool) -> Option<EditCommand> {
    if !mac || !k.primary || k.alt {
        return None;
    }
    Some(match (k.code, k.shift) {
        (vk::Z, false) => EditCommand::Undo,
        (vk::Z, true) => EditCommand::Redo,
        (vk::X, false) => EditCommand::Cut,
        (vk::C, false) => EditCommand::Copy,
        (vk::V, false) => EditCommand::Paste,
        (vk::A, false) => EditCommand::SelectAll,
        _ => return None,
    })
}
