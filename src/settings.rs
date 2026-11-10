//! Применение настроек (`prefs.rs`) к Chromium и команды страницы
//! `rustium://settings`.
//!
//! Куда что попадает:
//! - ключи командной строки (до запуска CEF, нужен перезапуск):
//!   аппаратное ускорение, автовоспроизведение;
//! - настройки профиля (preferences запроса по умолчанию): cookies,
//!   Do Not Track, HTTPS-Only, языки сайтов;
//! - настройки браузера (local state): безопасный DNS;
//! - content settings: JavaScript и всплывающие окна (по умолчанию и для
//!   сайтов), cookies;
//! - сами решаем в обработчиках: камера, микрофон, геолокация и прочие
//!   запросы (`shell/permissions.rs`), всплывающие окна, звук, загрузки.

use crate::prefs::{self, Cookies, PermKind, Prefs, Setting, SitePerm};
use cef::*;
use serde::{Deserialize, Serialize};

/// Ключи командной строки из настроек. Вызывается в браузерном процессе
/// до инициализации CEF; дочерние процессы получают их от него.
pub fn apply_switches(command_line: &mut CommandLine) {
    let p = prefs::get();
    if !p.hardware_acceleration {
        command_line.append_switch(Some(&CefString::from("disable-gpu")));
        command_line.append_switch(Some(&CefString::from("disable-gpu-compositing")));
    }
    let autoplay = match p.permissions.autoplay.default {
        Setting::Allow => "no-user-gesture-required",
        _ => "document-user-activation-required",
    };
    command_line.append_switch_with_value(
        Some(&CefString::from("autoplay-policy")),
        Some(&CefString::from(autoplay)),
    );
}

/// Применить все настройки после инициализации CEF.
pub fn apply_all() {
    let p = prefs::get();
    apply(&p, None);
}

/// Применить изменившиеся настройки.
pub fn apply_changes(new: &Prefs, old: &Prefs) {
    apply(new, Some(old));
}

fn apply(p: &Prefs, old: Option<&Prefs>) {
    let changed = |f: &dyn Fn(&Prefs) -> bool| old.is_none_or(|o| f(o) != f(p));
    let Some(context) = request_context_get_global_context() else {
        eprintln!("[settings] нет контекста запросов");
        return;
    };

    if old.is_none_or(|o| o.cookies != p.cookies) {
        // 0 — не блокировать сторонние cookies, 1 — блокировать.
        let third_party = matches!(p.cookies, Cookies::BlockThirdParty | Cookies::BlockAll);
        set_pref(&context, "profile.cookie_controls_mode", int(third_party as i32));
        let value = if p.cookies == Cookies::BlockAll {
            ContentSettingValues::BLOCK
        } else {
            ContentSettingValues::ALLOW
        };
        context.set_content_setting(None, None, ContentSettingTypes::COOKIES, value);
    }
    if changed(&|p| p.do_not_track) {
        set_pref(&context, "enable_do_not_track", boolean(p.do_not_track));
    }
    if changed(&|p| p.https_only) {
        set_pref(&context, "https_only_mode_enabled", boolean(p.https_only));
    }
    if old.is_none_or(|o| o.language != p.language) {
        let langs = prefs::accept_languages(&p.language);
        set_pref(&context, "intl.accept_languages", string(&langs));
    }
    if old.is_none_or(|o| o.secure_dns != p.secure_dns) {
        if let Some(global) = preference_manager_get_global() {
            let (mode, template) = if p.secure_dns.enabled {
                ("secure", p.secure_dns.template())
            } else {
                ("off", String::new())
            };
            set_pref(&global, "dns_over_https.templates", string(&template));
            set_pref(&global, "dns_over_https.mode", string(mode));
        }
    }

    // JavaScript и всплывающие окна: значение по умолчанию и исключения.
    // Всплывающие окна без жеста Chromium блокирует сам (до
    // on_before_popup), поэтому «Allow» тоже надо передать ему.
    for (kind, content_type) in [
        (PermKind::Javascript, ContentSettingTypes::JAVASCRIPT),
        (PermKind::Popups, ContentSettingTypes::POPUPS),
    ] {
        let perm = p.permissions.get(kind);
        let old_perm = old.map(|o| o.permissions.get(kind));
        apply_content_setting(&context, content_type, perm, old_perm);
    }
}

fn apply_content_setting(
    context: &RequestContext,
    content_type: ContentSettingTypes,
    perm: &SitePerm,
    old: Option<&SitePerm>,
) {
    if old.is_none_or(|o| o.default != perm.default) {
        let value = content_value(perm.default);
        context.set_content_setting(None, None, content_type, value);
    }
    let mut sites: Vec<(&String, ContentSettingValues)> = Vec::new();
    for origin in &perm.allow {
        sites.push((origin, ContentSettingValues::ALLOW));
    }
    for origin in &perm.block {
        sites.push((origin, ContentSettingValues::BLOCK));
    }
    if let Some(o) = old {
        // Убранные из списков сайты возвращаем к значению по умолчанию.
        for origin in o.allow.iter().chain(&o.block) {
            if !perm.allow.contains(origin) && !perm.block.contains(origin) {
                sites.push((origin, ContentSettingValues::DEFAULT));
            }
        }
    }
    for (origin, value) in sites {
        let url = CefString::from(format!("{origin}/").as_str());
        context.set_content_setting(Some(&url), Some(&url), content_type, value);
    }
}

fn content_value(setting: Setting) -> ContentSettingValues {
    match setting {
        Setting::Allow => ContentSettingValues::ALLOW,
        Setting::Block => ContentSettingValues::BLOCK,
        Setting::Ask => ContentSettingValues::ASK,
    }
}

/// И контекст запросов (настройки профиля), и глобальный менеджер
/// (local state) умеют менять настройки, но через разные трейты.
trait PrefTarget {
    fn can_set(&self, name: &CefString) -> bool;
    fn set(&self, name: &CefString, value: Option<&mut Value>, error: &mut CefString) -> bool;
}

impl PrefTarget for RequestContext {
    fn can_set(&self, name: &CefString) -> bool {
        self.can_set_preference(Some(name)) != 0
    }
    fn set(&self, name: &CefString, value: Option<&mut Value>, error: &mut CefString) -> bool {
        self.set_preference(Some(name), value, Some(error)) != 0
    }
}

impl PrefTarget for PreferenceManager {
    fn can_set(&self, name: &CefString) -> bool {
        self.can_set_preference(Some(name)) != 0
    }
    fn set(&self, name: &CefString, value: Option<&mut Value>, error: &mut CefString) -> bool {
        self.set_preference(Some(name), value, Some(error)) != 0
    }
}

fn set_pref(manager: &impl PrefTarget, name: &str, mut value: Option<Value>) {
    let name_cef = CefString::from(name);
    if !manager.can_set(&name_cef) {
        eprintln!("[settings] настройку {name} изменить нельзя");
        return;
    }
    // Пустая CefString уходит в CEF нулевым указателем, и текст ошибки
    // некуда записать: начинаем с непустой.
    let mut error = CefString::from("?");
    if !manager.set(&name_cef, value.as_mut(), &mut error) {
        eprintln!("[settings] {name}: {error}");
    }
}

fn boolean(v: bool) -> Option<Value> {
    let value = value_create()?;
    value.set_bool(v as i32);
    Some(value)
}

fn int(v: i32) -> Option<Value> {
    let value = value_create()?;
    value.set_int(v);
    Some(value)
}

fn string(v: &str) -> Option<Value> {
    let value = value_create()?;
    value.set_string(Some(&CefString::from(v)));
    Some(value)
}

/// Решение о разрешении для сайта.
pub fn decide(kind: PermKind, origin: &str) -> Setting {
    prefs::get().permissions.decide(kind, origin)
}

// -------------------------------------------------------------------------
// Страница настроек
// -------------------------------------------------------------------------

/// Команды страницы `rustium://settings`.
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum SettingsCommand {
    GetSettings,
    SetSettings { prefs: Box<Prefs> },
    ChooseDownloadDir,
    MakeDefaultBrowser,
    OpenProxySettings,
}

/// Сведения о браузере для страницы (`info`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub version: &'static str,
    pub chromium: String,
    pub cef: String,
    pub platform: &'static str,
    pub profile: String,
    pub downloads_default: String,
    pub default_browser: Option<bool>,
}

pub fn info() -> Info {
    Info {
        version: env!("CARGO_PKG_VERSION"),
        chromium: format!(
            "{}.{}.{}.{}",
            sys::CHROME_VERSION_MAJOR,
            sys::CHROME_VERSION_MINOR,
            sys::CHROME_VERSION_BUILD,
            sys::CHROME_VERSION_PATCH
        ),
        cef: format!(
            "{}.{}.{}",
            sys::CEF_VERSION_MAJOR,
            sys::CEF_VERSION_MINOR,
            sys::CEF_VERSION_PATCH
        ),
        platform: if cfg!(target_os = "macos") { "macos" } else { "linux" },
        profile: prefs::profile_dir().to_string_lossy().into_owned(),
        downloads_default: crate::downloads::downloads_dir().to_string_lossy().into_owned(),
        default_browser: is_default_browser(),
    }
}

/// Rustium — браузер по умолчанию? `None` — не умеем узнать.
pub fn is_default_browser() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        crate::mac::is_default_browser()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Попросить систему сделать Rustium браузером по умолчанию (macOS
/// показывает своё подтверждение).
pub fn make_default_browser() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        crate::mac::make_default_browser();
    }
    is_default_browser()
}

pub fn open_proxy_settings() {
    #[cfg(target_os = "macos")]
    crate::downloads::open_file(std::path::Path::new(
        "x-apple.systempreferences:com.apple.Network-Settings.extension?Proxies",
    ));
}

/// Ответ на `getSettings` / `setSettings`.
pub fn settings_json(with_info: bool) -> String {
    let prefs = prefs::get();
    let value = if with_info {
        serde_json::json!({ "prefs": prefs, "info": info() })
    } else {
        serde_json::json!({ "prefs": prefs })
    };
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_settings_commands() {
        let c: SettingsCommand = serde_json::from_str(r#"{"cmd":"getSettings"}"#).unwrap();
        assert!(matches!(c, SettingsCommand::GetSettings));
        let c: SettingsCommand =
            serde_json::from_str(r#"{"cmd":"setSettings","prefs":{"cookies":"blockAll"}}"#).unwrap();
        assert!(matches!(c, SettingsCommand::SetSettings { prefs } if prefs.cookies == Cookies::BlockAll));
        assert!(serde_json::from_str::<SettingsCommand>(r#"{"cmd":"quit"}"#).is_err());
        assert!(serde_json::from_str::<SettingsCommand>(r#"{"cmd":"retry"}"#).is_err());
    }
}
