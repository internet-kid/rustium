//! Превращает текст из адресной строки в URL: адрес сайта, localhost,
//! IP или поисковый запрос. Модуль не зависит от CEF, его можно
//! тестировать отдельно: `rustc --edition 2021 --test src/urlfix.rs`.

/// Поисковик по умолчанию. Запрос дописывается в конец (уже закодированным).
pub const SEARCH_URL: &str = "https://duckduckgo.com/?q=";

/// Схемы, которые открываем как есть, даже без `//`.
/// `javascript:` сюда сознательно не входит: вставка такого «адреса»
/// выполнила бы код в текущей странице (классический paste-jacking).
const PASSTHROUGH_SCHEMES: &[&str] = &[
    "about:",
    "data:",
    "file:",
    "view-source:",
    "chrome:",
    "mailto:",
    "blob:",
];

/// Нормализует ввод пользователя. `None` — пустой ввод.
pub fn fixup(input: &str) -> Option<String> {
    fixup_with(input, SEARCH_URL)
}

/// То же, но поиск — через `search_prefix` (запрос дописывается в конец).
pub fn fixup_with(input: &str, search_prefix: &str) -> Option<String> {
    let search_url = |q: &str| format!("{search_prefix}{}", encode_query(q));
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let has_space = s.chars().any(char::is_whitespace);
    let lower = s.to_ascii_lowercase();

    if !has_space {
        if lower.starts_with("javascript:") {
            return Some(search_url(s));
        }
        if let Some(scheme_end) = lower.find("://") {
            if is_scheme(&lower[..scheme_end]) {
                return Some(s.to_string());
            }
        }
        if PASSTHROUGH_SCHEMES.iter().any(|p| lower.starts_with(p)) {
            return Some(s.to_string());
        }
        let host = host_part(&lower);
        let (host_no_port, port) = split_port(host);
        if port == Some(false) {
            // "foo:bar" — двоеточие есть, но после него не порт.
            return Some(search_url(s));
        }
        if host_no_port == "localhost" || is_ipv4(host_no_port) {
            return Some(format!("http://{s}"));
        }
        if looks_like_domain(host_no_port) {
            return Some(format!("https://{s}"));
        }
    }
    Some(search_url(s))
}

/// URL поиска для произвольного текста.
pub fn search_url(query: &str) -> String {
    format!("{SEARCH_URL}{}", encode_query(query))
}

fn is_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Часть до первого `/`, `?` или `#`.
fn host_part(s: &str) -> &str {
    let end = s.find(['/', '?', '#']).unwrap_or(s.len());
    &s[..end]
}

/// Делит "host:port". Второе значение: `None` — порта нет,
/// `Some(true)` — корректный порт, `Some(false)` — после `:` не число.
fn split_port(host: &str) -> (&str, Option<bool>) {
    match host.rsplit_once(':') {
        None => (host, None),
        Some((h, p)) => {
            let ok = !p.is_empty() && p.len() <= 5 && p.chars().all(|c| c.is_ascii_digit());
            (h, Some(ok))
        }
    }
}

fn is_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
}

fn looks_like_domain(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let labels_ok = labels.iter().all(|l| {
        !l.is_empty()
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_alphanumeric() || c == '-')
    });
    let tld = labels[labels.len() - 1];
    labels_ok && tld.chars().count() >= 2 && tld.chars().all(char::is_alphabetic)
}

/// application/x-www-form-urlencoded: пробел -> `+`, остальное — %XX.
pub(crate) fn encode_query(q: &str) -> String {
    let mut out = String::with_capacity(q.len() * 3);
    for b in q.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: &str) -> String {
        fixup(s).unwrap()
    }

    #[test]
    fn empty() {
        assert_eq!(fixup("   "), None);
    }

    #[test]
    fn full_urls_pass_through() {
        assert_eq!(f("https://example.com/a?b=c"), "https://example.com/a?b=c");
        assert_eq!(f("  http://x.y  "), "http://x.y");
        assert_eq!(f("about:blank"), "about:blank");
        assert_eq!(f("file:///etc/hosts"), "file:///etc/hosts");
        assert_eq!(f("view-source:https://a.com"), "view-source:https://a.com");
    }

    #[test]
    fn domains_get_https() {
        assert_eq!(f("rust-lang.org"), "https://rust-lang.org");
        assert_eq!(f("docs.rs/cef"), "https://docs.rs/cef");
        assert_eq!(f("example.com:8443/x"), "https://example.com:8443/x");
        assert_eq!(f("яндекс.рф"), "https://яндекс.рф");
    }

    #[test]
    fn local_gets_http() {
        assert_eq!(f("localhost"), "http://localhost");
        assert_eq!(f("localhost:3000/api"), "http://localhost:3000/api");
        assert_eq!(f("127.0.0.1:8080"), "http://127.0.0.1:8080");
        assert_eq!(f("192.168.1.1"), "http://192.168.1.1");
    }

    #[test]
    fn everything_else_is_search() {
        assert_eq!(f("как сделать браузер"), format!("{SEARCH_URL}%D0%BA%D0%B0%D0%BA+%D1%81%D0%B4%D0%B5%D0%BB%D0%B0%D1%82%D1%8C+%D0%B1%D1%80%D0%B0%D1%83%D0%B7%D0%B5%D1%80"));
        assert_eq!(f("rust"), format!("{SEARCH_URL}rust"));
        assert_eq!(f("v1.2"), format!("{SEARCH_URL}v1.2"));
        assert_eq!(f("a.b"), format!("{SEARCH_URL}a.b"));
        assert_eq!(f("c++ & rust"), format!("{SEARCH_URL}c%2B%2B+%26+rust"));
        assert_eq!(f("foo:bar"), format!("{SEARCH_URL}foo%3Abar"));
    }

    #[test]
    fn javascript_is_never_executed() {
        assert_eq!(
            f("javascript:alert(1)"),
            format!("{SEARCH_URL}javascript%3Aalert%281%29")
        );
        assert_eq!(
            f("JavaScript://x"),
            format!("{SEARCH_URL}JavaScript%3A%2F%2Fx")
        );
    }
}
