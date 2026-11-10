//! Настройки браузера и сессия: модель, проверка, хранение на диске.
//!
//! Файлы в папке профиля:
//! - `rustium-settings.json` — настройки (`Prefs`);
//! - `rustium-session.json` — открытые вкладки для «Continue where you
//!   left off».
//!
//! Модуль не зависит от CEF. Применение настроек к CEF — в `settings.rs`.
//! Формат JSON совпадает с тем, что ждёт страница `ui/settings.html`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

// -------------------------------------------------------------------------
// Модель
// -------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Prefs {
    pub startup: Startup,
    pub search_engine: SearchEngine,
    /// Предпочитаемый язык сайтов (первый в Accept-Language).
    pub language: String,
    pub cookies: Cookies,
    pub do_not_track: bool,
    pub https_only: bool,
    pub secure_dns: SecureDns,
    pub downloads: DownloadPrefs,
    pub hardware_acceleration: bool,
    pub permissions: Permissions,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            startup: Startup::default(),
            search_engine: SearchEngine::DuckDuckGo,
            language: "en-US".into(),
            cookies: Cookies::BlockThirdParty,
            do_not_track: false,
            https_only: false,
            secure_dns: SecureDns::default(),
            downloads: DownloadPrefs::default(),
            hardware_acceleration: true,
            permissions: Permissions::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Startup {
    pub mode: StartupMode,
    pub pages: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StartupMode {
    #[default]
    Restore,
    NewTab,
    Pages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchEngine {
    #[serde(rename = "duckduckgo")]
    DuckDuckGo,
    Google,
    Bing,
    Brave,
    Startpage,
    Ecosia,
}

impl SearchEngine {
    pub fn label(self) -> &'static str {
        match self {
            SearchEngine::DuckDuckGo => "DuckDuckGo",
            SearchEngine::Google => "Google",
            SearchEngine::Bing => "Bing",
            SearchEngine::Brave => "Brave Search",
            SearchEngine::Startpage => "Startpage",
            SearchEngine::Ecosia => "Ecosia",
        }
    }

    /// Адрес поиска; запрос дописывается в конец.
    pub fn search_prefix(self) -> &'static str {
        match self {
            SearchEngine::DuckDuckGo => "https://duckduckgo.com/?q=",
            SearchEngine::Google => "https://www.google.com/search?q=",
            SearchEngine::Bing => "https://www.bing.com/search?q=",
            SearchEngine::Brave => "https://search.brave.com/search?q=",
            SearchEngine::Startpage => "https://www.startpage.com/do/search?q=",
            SearchEngine::Ecosia => "https://www.ecosia.org/search?q=",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Cookies {
    AllowAll,
    BlockThirdParty,
    BlockAll,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SecureDns {
    pub enabled: bool,
    pub provider: DnsProvider,
    /// Шаблон DoH для `Custom`.
    pub custom: String,
}

impl Default for SecureDns {
    fn default() -> Self {
        SecureDns {
            enabled: false,
            provider: DnsProvider::Cloudflare,
            custom: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DnsProvider {
    Cloudflare,
    Google,
    Quad9,
    Custom,
}

impl SecureDns {
    /// Шаблон DoH для Chromium (`dns_over_https.templates`).
    pub fn template(&self) -> String {
        match self.provider {
            DnsProvider::Cloudflare => "https://chrome.cloudflare-dns.com/dns-query".into(),
            DnsProvider::Google => "https://dns.google/dns-query{?dns}".into(),
            DnsProvider::Quad9 => "https://dns.quad9.net/dns-query".into(),
            DnsProvider::Custom => self.custom.trim().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DownloadPrefs {
    /// `None` — папка «Загрузки» пользователя.
    pub dir: Option<String>,
    /// Спрашивать, куда сохранять каждый файл.
    pub ask: bool,
}

/// Что сайт может делать без вопроса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Setting {
    Ask,
    Allow,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermKind {
    Camera,
    Microphone,
    Location,
    Notifications,
    Clipboard,
    Popups,
    Javascript,
    Sound,
    Autoplay,
}

impl PermKind {
    pub const ALL: [PermKind; 9] = [
        PermKind::Camera,
        PermKind::Microphone,
        PermKind::Location,
        PermKind::Notifications,
        PermKind::Clipboard,
        PermKind::Popups,
        PermKind::Javascript,
        PermKind::Sound,
        PermKind::Autoplay,
    ];

    /// Разрешения, о которых сайт спрашивает пользователя
    /// (по умолчанию «Ask» или «Block»). Остальные — «Allow» или «Block».
    pub fn asks(self) -> bool {
        matches!(
            self,
            PermKind::Camera
                | PermKind::Microphone
                | PermKind::Location
                | PermKind::Notifications
                | PermKind::Clipboard
        )
    }

    /// Есть ли у разрешения исключения для сайтов.
    pub fn has_exceptions(self) -> bool {
        self != PermKind::Autoplay
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SitePerm {
    pub default: Setting,
    /// Origin'ы вида `https://host[:port]`.
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub block: Vec<String>,
}

impl SitePerm {
    fn new(default: Setting) -> Self {
        SitePerm {
            default,
            allow: Vec::new(),
            block: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Permissions {
    pub camera: SitePerm,
    pub microphone: SitePerm,
    pub location: SitePerm,
    pub notifications: SitePerm,
    pub clipboard: SitePerm,
    pub popups: SitePerm,
    pub javascript: SitePerm,
    pub sound: SitePerm,
    pub autoplay: SitePerm,
}

impl Default for Permissions {
    fn default() -> Self {
        Permissions {
            camera: SitePerm::new(Setting::Ask),
            microphone: SitePerm::new(Setting::Ask),
            location: SitePerm::new(Setting::Ask),
            notifications: SitePerm::new(Setting::Block),
            clipboard: SitePerm::new(Setting::Ask),
            popups: SitePerm::new(Setting::Block),
            javascript: SitePerm::new(Setting::Allow),
            sound: SitePerm::new(Setting::Allow),
            autoplay: SitePerm::new(Setting::Block),
        }
    }
}

impl Permissions {
    pub fn get(&self, kind: PermKind) -> &SitePerm {
        match kind {
            PermKind::Camera => &self.camera,
            PermKind::Microphone => &self.microphone,
            PermKind::Location => &self.location,
            PermKind::Notifications => &self.notifications,
            PermKind::Clipboard => &self.clipboard,
            PermKind::Popups => &self.popups,
            PermKind::Javascript => &self.javascript,
            PermKind::Sound => &self.sound,
            PermKind::Autoplay => &self.autoplay,
        }
    }

    pub fn get_mut(&mut self, kind: PermKind) -> &mut SitePerm {
        match kind {
            PermKind::Camera => &mut self.camera,
            PermKind::Microphone => &mut self.microphone,
            PermKind::Location => &mut self.location,
            PermKind::Notifications => &mut self.notifications,
            PermKind::Clipboard => &mut self.clipboard,
            PermKind::Popups => &mut self.popups,
            PermKind::Javascript => &mut self.javascript,
            PermKind::Sound => &mut self.sound,
            PermKind::Autoplay => &mut self.autoplay,
        }
    }

    /// Решение для сайта: исключение, иначе значение по умолчанию.
    pub fn decide(&self, kind: PermKind, origin: &str) -> Setting {
        let perm = self.get(kind);
        let Some(origin) = normalize_origin(origin).ok() else {
            return perm.default;
        };
        if kind.has_exceptions() {
            if perm.block.contains(&origin) {
                return Setting::Block;
            }
            if perm.allow.contains(&origin) {
                return Setting::Allow;
            }
        }
        perm.default
    }

    /// Запомнить ответ пользователя для сайта.
    pub fn remember(&mut self, kind: PermKind, origin: &str, allow: bool) {
        let Ok(origin) = normalize_origin(origin) else { return };
        let perm = self.get_mut(kind);
        perm.allow.retain(|o| *o != origin);
        perm.block.retain(|o| *o != origin);
        if allow {
            perm.allow.push(origin);
        } else {
            perm.block.push(origin);
        }
    }
}

/// Языки для заголовка Accept-Language (`intl.accept_languages`).
pub fn accept_languages(language: &str) -> String {
    let mut list = vec![language.to_string()];
    // Базовый язык без региона: "pt-BR" -> "pt".
    if let Some((base, _)) = language.split_once('-') {
        list.push(base.to_string());
    }
    for fallback in ["en-US", "en"] {
        if !list.iter().any(|l| l == fallback) {
            list.push(fallback.to_string());
        }
    }
    list.join(",")
}

const LANGUAGES: &[&str] = &[
    "en-US", "en-GB", "ru", "uk", "de", "fr", "es", "it", "pt-BR", "pl", "tr", "zh-CN", "ja", "ko",
];

// -------------------------------------------------------------------------
// Проверка
// -------------------------------------------------------------------------

/// Проверить и привести к каноническому виду настройки, пришедшие со
/// страницы. `old` — текущие настройки: сайт, только что добавленный в
/// один список разрешения, убирается из другого.
pub fn normalize(mut new: Prefs, old: &Prefs) -> Result<Prefs, String> {
    // Стартовые страницы: только http(s), без дублей.
    let mut pages = Vec::new();
    for page in &new.startup.pages {
        let url = normalize_page(page)?;
        if !pages.contains(&url) {
            pages.push(url);
        }
    }
    new.startup.pages = pages;

    if !LANGUAGES.contains(&new.language.as_str()) {
        return Err("Unknown language".into());
    }

    new.secure_dns.custom = new.secure_dns.custom.trim().to_string();
    let custom = &new.secure_dns.custom;
    if !custom.is_empty() && (!custom.starts_with("https://") || custom.contains(char::is_whitespace)) {
        return Err("A secure DNS address must start with https://".into());
    }
    if new.secure_dns.enabled
        && new.secure_dns.provider == DnsProvider::Custom
        && new.secure_dns.custom.is_empty()
    {
        // Включили «Custom», но адрес ещё не ввели: пока не включаем.
        new.secure_dns.enabled = false;
    }

    if let Some(dir) = &new.downloads.dir {
        let dir = dir.trim();
        if dir.is_empty() {
            new.downloads.dir = None;
        } else if !Path::new(dir).is_absolute() {
            return Err("The download folder must be a full path".into());
        }
    }

    for kind in PermKind::ALL {
        let old_perm = old.permissions.get(kind).clone();
        let perm = new.permissions.get_mut(kind);
        let valid = if kind.asks() {
            perm.default != Setting::Allow
        } else {
            perm.default != Setting::Ask
        };
        if !valid {
            return Err("This permission can’t use that default".into());
        }
        if !kind.has_exceptions() {
            perm.allow.clear();
            perm.block.clear();
            continue;
        }
        let allow = normalize_list(&perm.allow)?;
        let block = normalize_list(&perm.block)?;
        // Сайт в обоих списках: побеждает список, куда его добавили сейчас.
        let (allow, block) = (
            allow
                .iter()
                .filter(|o| !block.contains(o) || !old_perm.allow.contains(o))
                .cloned()
                .collect::<Vec<_>>(),
            block
                .iter()
                .filter(|o| !allow.contains(o) || old_perm.allow.contains(o))
                .cloned()
                .collect::<Vec<_>>(),
        );
        perm.allow = allow;
        perm.block = block;
    }
    Ok(new)
}

fn normalize_list(list: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for item in list {
        let origin = normalize_origin(item)?;
        if !out.contains(&origin) {
            out.push(origin);
        }
    }
    Ok(out)
}

fn normalize_page(input: &str) -> Result<String, String> {
    let s = input.trim();
    let url = if s.contains("://") {
        s.to_string()
    } else {
        format!("https://{s}")
    };
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err("Start pages must be web addresses".into());
    }
    let origin = normalize_origin(&url).map_err(|_| "Not a valid page address".to_string())?;
    let rest = &url[url.find("://").unwrap() + 3..];
    let path = rest.find(['/', '?', '#']).map_or("/", |i| &rest[i..]);
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    Ok(format!("{origin}{path}"))
}

/// `meet.example.com`, `https://meet.example.com/x` → `https://meet.example.com`.
/// Порт сохраняется, путь отбрасывается. Без схемы — https
/// (для localhost и IP — http).
pub fn normalize_origin(input: &str) -> Result<String, String> {
    const BAD: &str = "Not a valid site address";
    let s = input.trim();
    let (scheme, rest) = match s.split_once("://") {
        Some((scheme, rest)) => (Some(scheme.to_ascii_lowercase()), rest),
        None => (None, s),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // user:pass@host — не принимаем.
    if authority.contains('@') {
        return Err(BAD.into());
    }
    let authority = authority.to_lowercase();
    let (host, port) = match authority.rsplit_once(':') {
        // "[::1]" — двоеточия внутри IPv6, порта нет.
        Some((h, p)) if !authority.ends_with(']') => {
            if p.is_empty() || p.len() > 5 || !p.chars().all(|c| c.is_ascii_digit()) {
                return Err(BAD.into());
            }
            (h.to_string(), Some(p.to_string()))
        }
        _ => (authority.clone(), None),
    };
    let host = host.trim_end_matches('.').to_string();
    let local = host == "localhost" || is_ip(&host);
    let valid_host = local
        || (host.contains('.')
            && host.split('.').all(|label| {
                !label.is_empty()
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.chars().all(|c| c.is_alphanumeric() || c == '-')
            })
            && host.rsplit('.').next().is_some_and(|tld| tld.chars().all(char::is_alphabetic)));
    if !valid_host {
        return Err(BAD.into());
    }
    let scheme = match scheme {
        Some(s) if s == "http" || s == "https" => s,
        Some(_) => return Err(BAD.into()),
        None if local => "http".into(),
        None => "https".into(),
    };
    Ok(match port {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

fn is_ip(host: &str) -> bool {
    host.parse::<std::net::Ipv4Addr>().is_ok()
        || host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .is_some_and(|h| h.parse::<std::net::Ipv6Addr>().is_ok())
}

// -------------------------------------------------------------------------
// Хранение
// -------------------------------------------------------------------------

const PREFS_FILE: &str = "rustium-settings.json";
const SESSION_FILE: &str = "rustium-session.json";

struct Store {
    dir: PathBuf,
    prefs: Prefs,
    /// Сессия больше не сохраняется (идёт выход).
    session_frozen: bool,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> MutexGuard<'static, Store> {
    STORE
        .get_or_init(|| {
            Mutex::new(Store {
                dir: std::env::temp_dir(),
                prefs: Prefs::default(),
                session_frozen: true,
            })
        })
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Прочитать настройки из папки профиля. Вызывать до инициализации CEF:
/// часть настроек — ключи командной строки Chromium.
pub fn init(profile: &Path) {
    let prefs = std::fs::read(profile.join(PREFS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Prefs>(&bytes).ok())
        .and_then(|p| normalize(p, &Prefs::default()).ok())
        .unwrap_or_default();
    let fresh = Store {
        dir: profile.to_path_buf(),
        prefs,
        session_frozen: false,
    };
    if STORE.set(Mutex::new(fresh)).is_err() {
        // Уже создано (например, в тестах): переписываем.
        let mut s = store();
        s.dir = profile.to_path_buf();
        s.session_frozen = false;
    }
}

/// Текущие настройки (копия).
pub fn get() -> Prefs {
    store().prefs.clone()
}

/// Папка профиля.
pub fn profile_dir() -> PathBuf {
    store().dir.clone()
}

/// Проверить, сохранить и вернуть новые настройки. Второе значение —
/// предыдущие (для применения изменений).
pub fn set(new: Prefs) -> Result<(Prefs, Prefs), String> {
    let mut s = store();
    let old = s.prefs.clone();
    let new = normalize(new, &old)?;
    let json = serde_json::to_vec_pretty(&new).map_err(|e| e.to_string())?;
    write_atomic(&s.dir.join(PREFS_FILE), &json)
        .map_err(|e| format!("Couldn’t save settings: {e}"))?;
    s.prefs = new.clone();
    Ok((new, old))
}

/// Изменить настройки функцией (например, запомнить ответ на запрос).
pub fn update(f: impl FnOnce(&mut Prefs)) -> Result<(Prefs, Prefs), String> {
    let mut new = get();
    f(&mut new);
    set(new)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub tabs: Vec<String>,
    pub active: usize,
}

/// Сохранить открытые вкладки (если не идёт выход).
pub fn save_session(session: &Session) {
    let s = store();
    if s.session_frozen {
        return;
    }
    if let Ok(json) = serde_json::to_vec(session) {
        let _ = write_atomic(&s.dir.join(SESSION_FILE), &json);
    }
}

/// Больше не сохранять сессию: начался выход, вкладки закрываются
/// одна за другой, и последнее сохранённое состояние — правильное.
pub fn freeze_session() {
    store().session_frozen = true;
}

pub fn load_session() -> Session {
    let dir = store().dir.clone();
    std::fs::read(dir.join(SESSION_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_matches_the_page() {
        let json = serde_json::to_value(Prefs::default()).unwrap();
        assert_eq!(json["startup"]["mode"], "restore");
        assert_eq!(json["searchEngine"], "duckduckgo");
        assert_eq!(json["cookies"], "blockThirdParty");
        assert_eq!(json["secureDns"]["provider"], "cloudflare");
        assert_eq!(json["downloads"]["dir"], serde_json::Value::Null);
        assert_eq!(json["permissions"]["notifications"]["default"], "block");
        assert_eq!(json["permissions"]["javascript"]["allow"], serde_json::json!([]));
        // Неполный файл дополняется значениями по умолчанию.
        let p: Prefs = serde_json::from_str(r#"{"cookies":"blockAll"}"#).unwrap();
        assert_eq!(p.cookies, Cookies::BlockAll);
        assert_eq!(p.permissions.camera.default, Setting::Ask);
        // Неизвестные значения не проходят.
        assert!(serde_json::from_str::<Prefs>(r#"{"cookies":"maybe"}"#).is_err());
    }

    #[test]
    fn origins() {
        let n = |s| normalize_origin(s);
        assert_eq!(n("meet.example.com").unwrap(), "https://meet.example.com");
        assert_eq!(n("HTTPS://Meet.Example.com/x?y").unwrap(), "https://meet.example.com");
        assert_eq!(n("http://example.com:8080/").unwrap(), "http://example.com:8080");
        assert_eq!(n("localhost:3000").unwrap(), "http://localhost:3000");
        assert_eq!(n("127.0.0.1").unwrap(), "http://127.0.0.1");
        assert_eq!(n("яндекс.рф").unwrap(), "https://яндекс.рф");
        assert!(n("hello").is_err());
        assert!(n("ftp://example.com").is_err());
        assert!(n("javascript:alert(1)").is_err());
        assert!(n("user@example.com").is_err());
        assert!(n("example.com:http").is_err());
        assert!(n("").is_err());
    }

    #[test]
    fn startup_pages() {
        assert_eq!(normalize_page("example.com").unwrap(), "https://example.com/");
        assert_eq!(normalize_page("http://a.org/x?y=1").unwrap(), "http://a.org/x?y=1");
        assert!(normalize_page("javascript:alert(1)").is_err());
        assert!(normalize_page("file:///etc/passwd").is_err());
    }

    #[test]
    fn normalize_moves_sites_between_lists() {
        let mut old = Prefs::default();
        old.permissions.camera.allow = vec!["https://a.com".into()];
        // Страница добавила a.com в block, а из allow убрать забыла.
        let mut new = old.clone();
        new.permissions.camera.block = vec!["a.com".into()];
        let p = normalize(new, &old).unwrap();
        assert_eq!(p.permissions.camera.allow, Vec::<String>::new());
        assert_eq!(p.permissions.camera.block, vec!["https://a.com"]);
        // И обратно.
        let old = p.clone();
        let mut new = p;
        new.permissions.camera.allow.push("https://a.com".into());
        let p = normalize(new, &old).unwrap();
        assert_eq!(p.permissions.camera.allow, vec!["https://a.com"]);
        assert!(p.permissions.camera.block.is_empty());
    }

    #[test]
    fn normalize_rejects_bad_values() {
        let old = Prefs::default();
        let mut p = old.clone();
        p.permissions.camera.default = Setting::Allow;
        assert!(normalize(p, &old).is_err());
        let mut p = old.clone();
        p.permissions.popups.default = Setting::Ask;
        assert!(normalize(p, &old).is_err());
        let mut p = old.clone();
        p.permissions.popups.allow = vec!["not a site".into()];
        assert!(normalize(p, &old).is_err());
        let mut p = old.clone();
        p.secure_dns.custom = "http://dns.example".into();
        assert!(normalize(p, &old).is_err());
        let mut p = old.clone();
        p.language = "xx".into();
        assert!(normalize(p, &old).is_err());
        let mut p = old.clone();
        p.downloads.dir = Some("relative/path".into());
        assert!(normalize(p, &old).is_err());
    }

    #[test]
    fn decisions() {
        let mut perms = Permissions::default();
        assert_eq!(perms.decide(PermKind::Camera, "https://a.com"), Setting::Ask);
        perms.remember(PermKind::Camera, "https://a.com/", true);
        assert_eq!(perms.decide(PermKind::Camera, "https://a.com"), Setting::Allow);
        perms.remember(PermKind::Camera, "https://a.com", false);
        assert_eq!(perms.decide(PermKind::Camera, "https://a.com"), Setting::Block);
        assert!(perms.camera.allow.is_empty());
        assert_eq!(perms.decide(PermKind::Javascript, "https://b.com"), Setting::Allow);
    }

    #[test]
    fn languages() {
        assert_eq!(accept_languages("en-US"), "en-US,en");
        assert_eq!(accept_languages("ru"), "ru,en-US,en");
        assert_eq!(accept_languages("pt-BR"), "pt-BR,pt,en-US,en");
    }
}
