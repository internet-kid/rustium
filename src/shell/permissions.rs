//! Запросы разрешений сайтов: камера, микрофон, экран, геолокация,
//! уведомления, буфер обмена и прочие.
//!
//! CEF спрашивает через `PermissionHandler` (client.rs):
//! - `on_request_media_access_permission` — getUserMedia/getDisplayMedia;
//! - `on_show_permission_prompt` — остальные (битовая маска типов).
//!
//! Решение по настройкам (`prefs.rs`): исключение для сайта, иначе
//! значение по умолчанию. «Ask» ставит запрос в очередь вкладки; первый
//! запрос активной вкладки показывается всплывающим окном под значком
//! сайта (`ui/popup.html`, вид `permission`). Если окно закрыли, не
//! ответив, запрос ждёт: в адресной строке остаётся значок, по нему окно
//! открывается снова.

use super::{Shell, TabId};
use crate::prefs::{self, PermKind, Setting};
use cef::*;
use serde::Serialize;

// Биты cef_media_access_permission_types_t.
const MEDIA_AUDIO: u32 = 1;
const MEDIA_VIDEO: u32 = 2;
const MEDIA_DESKTOP_AUDIO: u32 = 4;
const MEDIA_DESKTOP_VIDEO: u32 = 8;

// Биты cef_permission_request_types_t, которые мы различаем.
const PERM_CAMERA: u32 = 4;
const PERM_CLIPBOARD: u32 = 16;
const PERM_GEOLOCATION: u32 = 256;
const PERM_MIC: u32 = 4096;
const PERM_NOTIFICATIONS: u32 = 32768;

/// Что сайт просит — строка в окне запроса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Item {
    Camera,
    Microphone,
    Screen,
    Location,
    Notifications,
    Clipboard,
    /// Остальное: шрифты, MIDI, управление окнами и т.п.
    Other,
}

impl Item {
    /// Разрешение, под которым ответ запоминается. `None` — спрашиваем
    /// каждый раз (экран и редкие разрешения).
    fn kind(self) -> Option<PermKind> {
        match self {
            Item::Camera => Some(PermKind::Camera),
            Item::Microphone => Some(PermKind::Microphone),
            Item::Location => Some(PermKind::Location),
            Item::Notifications => Some(PermKind::Notifications),
            Item::Clipboard => Some(PermKind::Clipboard),
            Item::Screen | Item::Other => None,
        }
    }
}

/// Колбэк CEF, которому нужен ответ.
pub(crate) enum Reply {
    /// getUserMedia: `requested` — запрошенные биты.
    Media {
        callback: MediaAccessCallback,
        requested: u32,
    },
    Prompt {
        callback: PermissionPromptCallback,
        cef_id: u64,
    },
}

pub(crate) struct Pending {
    id: u64,
    pub(super) tab: TabId,
    origin: String,
    items: Vec<Item>,
    reply: Reply,
}

/// Запрос для всплывающего окна (`permission` в состоянии UI).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptView {
    pub id: u64,
    /// Хост сайта для заголовка «… wants to».
    pub host: String,
    pub items: Vec<Item>,
    /// Показывать «Remember this decision».
    pub can_remember: bool,
}

impl Pending {
    pub(crate) fn view(&self) -> PromptView {
        PromptView {
            id: self.id,
            host: super::host_of(&self.origin),
            items: self.items.clone(),
            can_remember: self.items.iter().any(|i| i.kind().is_some()),
        }
    }
}

/// Решение для набора пунктов по настройкам: `Some(true/false)` — ответить
/// сразу, `None` — спросить пользователя (`ask` — какие пункты).
fn decide_items(origin: &str, items: &[Item]) -> (Vec<Item>, Vec<Item>, Vec<Item>) {
    let perms = prefs::get().permissions;
    let (mut allowed, mut blocked, mut ask) = (Vec::new(), Vec::new(), Vec::new());
    for &item in items {
        match item.kind().map(|k| perms.decide(k, origin)) {
            Some(Setting::Allow) => allowed.push(item),
            Some(Setting::Block) => blocked.push(item),
            Some(Setting::Ask) | None => ask.push(item),
        }
    }
    (allowed, blocked, ask)
}

fn media_bits(item: Item) -> u32 {
    match item {
        Item::Camera => MEDIA_VIDEO,
        Item::Microphone => MEDIA_AUDIO,
        Item::Screen => MEDIA_DESKTOP_VIDEO | MEDIA_DESKTOP_AUDIO,
        _ => 0,
    }
}

impl Shell {
    /// getUserMedia / getDisplayMedia. Возвращает true: ответим сами.
    pub(crate) fn media_request(
        &self,
        tab: TabId,
        origin: String,
        requested: u32,
        callback: MediaAccessCallback,
    ) -> bool {
        debug(format_args!("media {origin} {requested:#x}"));
        let mut items = Vec::new();
        if requested & MEDIA_VIDEO != 0 {
            items.push(Item::Camera);
        }
        if requested & MEDIA_AUDIO != 0 {
            items.push(Item::Microphone);
        }
        if requested & (MEDIA_DESKTOP_VIDEO | MEDIA_DESKTOP_AUDIO) != 0 {
            items.push(Item::Screen);
        }
        let (allowed, blocked, ask) = decide_items(&origin, &items);
        let granted = allowed.iter().map(|&i| media_bits(i)).fold(0, |a, b| a | b) & requested;
        if !blocked.is_empty() {
            // Хотя бы что-то запрещено: getUserMedia с частичным
            // разрешением всё равно не сработает.
            callback.cont(0);
            return true;
        }
        if ask.is_empty() {
            callback.cont(granted);
            return true;
        }
        self.queue_prompt(tab, origin, ask, Reply::Media { callback, requested });
        true
    }

    /// Остальные разрешения. Возвращает true: ответим сами.
    pub(crate) fn permission_prompt(
        &self,
        tab: TabId,
        cef_id: u64,
        origin: String,
        requested: u32,
        callback: PermissionPromptCallback,
    ) -> bool {
        debug(format_args!("prompt #{cef_id} {origin} {requested:#x}"));
        let mut items = Vec::new();
        let known = [
            (PERM_CAMERA, Item::Camera),
            (PERM_MIC, Item::Microphone),
            (PERM_GEOLOCATION, Item::Location),
            (PERM_NOTIFICATIONS, Item::Notifications),
            (PERM_CLIPBOARD, Item::Clipboard),
        ];
        let mut rest = requested;
        for (bit, item) in known {
            if requested & bit != 0 {
                items.push(item);
                rest &= !bit;
            }
        }
        if rest != 0 {
            items.push(Item::Other);
        }
        let (_, blocked, ask) = decide_items(&origin, &items);
        if !blocked.is_empty() {
            callback.cont(PermissionRequestResult::DENY);
            return true;
        }
        if ask.is_empty() {
            callback.cont(PermissionRequestResult::ACCEPT);
            return true;
        }
        self.queue_prompt(tab, origin, ask, Reply::Prompt { callback, cef_id });
        true
    }

    fn queue_prompt(&self, tab: TabId, origin: String, items: Vec<Item>, reply: Reply) {
        let show = {
            let mut s = self.lock();
            s.next_prompt += 1;
            let id = s.next_prompt;
            s.prompts.push(Pending { id, tab, origin, items, reply });
            s.active == Some(tab) && s.popup.is_none()
        };
        self.push_state();
        if show {
            self.open_permission_popup();
        }
    }

    /// Попросить панель открыть окно запроса под значком сайта: только
    /// она знает, где этот значок.
    pub(super) fn open_permission_popup(&self) {
        self.run_in_toolbar("window.rustium && window.rustium.openPermission && window.rustium.openPermission();");
    }

    /// Есть ли у вкладки неотвеченный запрос.
    pub(super) fn has_prompt(&self, tab: TabId) -> bool {
        self.lock().prompts.iter().any(|p| p.tab == tab)
    }

    /// Пользователь ответил в окне запроса.
    pub(super) fn permission_decision(&self, id: u64, allow: bool, remember: bool) {
        let (pending, next) = {
            let mut s = self.lock();
            let Some(pos) = s.prompts.iter().position(|p| p.id == id) else {
                return;
            };
            let pending = s.prompts.remove(pos);
            let next = s.prompts.iter().any(|p| p.tab == pending.tab) && s.active == Some(pending.tab);
            (pending, next)
        };
        if remember {
            let kinds: Vec<PermKind> = pending.items.iter().filter_map(|i| i.kind()).collect();
            if !kinds.is_empty() {
                let origin = pending.origin.clone();
                let result = prefs::update(|p| {
                    for kind in kinds {
                        p.permissions.remember(kind, &origin, allow);
                    }
                });
                if let Err(err) = result {
                    eprintln!("rustium: {err}");
                }
            }
        }
        answer(pending.reply, allow);
        self.push_state();
        if next {
            self.open_permission_popup();
        }
    }

    /// CEF отменил запрос (например, страница ушла).
    pub(crate) fn permission_dismissed(&self, cef_id: u64) {
        let removed = {
            let mut s = self.lock();
            let before = s.prompts.len();
            s.prompts
                .retain(|p| !matches!(p.reply, Reply::Prompt { cef_id: id, .. } if id == cef_id));
            before != s.prompts.len()
        };
        if removed {
            self.close_permission_popup_if_stale();
            self.push_state();
        }
    }

    /// Вкладка ушла на другой сайт или закрылась: её запросы больше не
    /// нужны. `keep_origin` — сайт, запросы которого оставить.
    pub(crate) fn drop_prompts(&self, tab: TabId, keep_origin: Option<&str>) {
        let dropped: Vec<Pending> = {
            let mut s = self.lock();
            let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut s.prompts)
                .into_iter()
                .partition(|p| {
                    p.tab == tab && keep_origin.is_none_or(|o| super::host_of(o) != super::host_of(&p.origin))
                });
            s.prompts = kept;
            gone
        };
        if dropped.is_empty() {
            return;
        }
        for pending in dropped {
            match pending.reply {
                Reply::Media { callback, .. } => callback.cancel(),
                Reply::Prompt { callback, .. } => callback.cont(PermissionRequestResult::DISMISS),
            }
        }
        self.close_permission_popup_if_stale();
        self.push_state();
    }

    /// Окно запроса открыто, а отвечать больше не на что — закрыть.
    fn close_permission_popup_if_stale(&self) {
        let stale = {
            let s = self.lock();
            s.popup == Some(crate::ipc::PopupKind::Permission)
                && !s.prompts.iter().any(|p| Some(p.tab) == s.active)
        };
        if stale {
            self.close_popup(false);
        }
    }

    /// Камера или микрофон вкладки включились или выключились.
    pub(crate) fn media_access_changed(&self, tab: TabId, video: bool, audio: bool) {
        self.update_tab(tab, |t| {
            t.camera_on = video;
            t.mic_on = audio;
        });
    }

    /// Звук сайта: выключить, если сайту он запрещён.
    pub(crate) fn apply_sound(&self, browser: &Browser, url: &str) {
        let blocked = url.starts_with("http")
            && crate::settings::decide(PermKind::Sound, url) == Setting::Block;
        if let Some(host) = browser.host() {
            if (host.is_audio_muted() != 0) != blocked {
                host.set_audio_muted(blocked as i32);
            }
        }
    }

    /// Настройки изменились: применить звук ко всем вкладкам.
    pub(crate) fn reapply_site_settings(&self) {
        let tabs: Vec<(Browser, String)> = self
            .lock()
            .tabs
            .iter()
            .filter_map(|t| t.browser.clone().map(|b| (b, t.url.clone())))
            .collect();
        for (browser, url) in tabs {
            self.apply_sound(&browser, &url);
        }
    }
}

/// RUSTIUM_DEBUG_PERMISSIONS=1 — печатать запросы разрешений.
fn debug(args: std::fmt::Arguments) {
    if std::env::var_os("RUSTIUM_DEBUG_PERMISSIONS").is_some() {
        eprintln!("[permissions] {args}");
    }
}

/// Ответить CEF.
fn answer(reply: Reply, allow: bool) {
    match reply {
        Reply::Media { callback, requested } => callback.cont(if allow { requested } else { 0 }),
        Reply::Prompt { callback, .. } => callback.cont(if allow {
            PermissionRequestResult::ACCEPT
        } else {
            PermissionRequestResult::DENY
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_map_to_settings() {
        assert_eq!(Item::Camera.kind(), Some(PermKind::Camera));
        assert_eq!(Item::Screen.kind(), None);
        assert_eq!(media_bits(Item::Screen), MEDIA_DESKTOP_VIDEO | MEDIA_DESKTOP_AUDIO);
        let json = serde_json::to_string(&Item::Notifications).unwrap();
        assert_eq!(json, "\"notifications\"");
    }
}
