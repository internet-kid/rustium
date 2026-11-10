//! Встроенные страницы интерфейса.
//!
//! Панель и всплывающие окна вшиваются в бинарник и открываются как
//! `data:`-URL. Новая вкладка и страницы ошибок живут на схеме
//! `rustium://` (scheme.rs).

use std::sync::OnceLock;

/// Высота панели (вкладки + адресная строка) в логических пикселях.
/// Должна совпадать с `--toolbar-height` в `ui/toolbar.html`.
pub const TOOLBAR_HEIGHT: i32 = 80;

/// Заголовок вкладки, пока страница не прислала свой.
pub const NEW_TAB_TITLE: &str = "New Tab";

const TOOLBAR_HTML: &str = include_str!("../ui/toolbar.html");
const POPUP_HTML: &str = include_str!("../ui/popup.html");
const SPLITTER_HTML: &str = include_str!("../ui/splitter.html");

/// Geist (SIL Open Font License) — шрифт интерфейса по макетам. Вшивается
/// в страницы как data-URL: панель не грузит ничего из сети.
const GEIST: &[u8] = include_bytes!("../ui/fonts/Geist-Variable.ttf");
/// Метка в CSS, вместо которой подставляется `@font-face` с Geist.
const FONT_MARKER: &str = "/*@font-geist*/";

pub fn toolbar_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| data_url(&with_fonts(TOOLBAR_HTML)))
}

/// Разделитель между страницей и встроенными DevTools.
pub fn splitter_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| data_url(SPLITTER_HTML))
}

/// Всплывающие окна поверх страницы: меню и загрузки.
pub fn popup_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| data_url(&with_fonts(POPUP_HTML)))
}

pub fn newtab_url() -> &'static str {
    crate::scheme::NEWTAB_URL
}

fn with_fonts(html: &str) -> String {
    static FACE: OnceLock<String> = OnceLock::new();
    let face = FACE.get_or_init(|| {
        format!(
            "@font-face {{ font-family: Geist; font-weight: 100 900; font-display: block; \
             src: url(data:font/ttf;base64,{}) format('truetype'); }}",
            base64(GEIST)
        )
    });
    html.replacen(FONT_MARKER, face, 1)
}

pub fn is_newtab(url: &str) -> bool {
    url.trim_end_matches('/') == newtab_url().trim_end_matches('/')
}

/// URL так, как его видит пользователь: для служебной новой вкладки — пусто.
pub fn display_url(url: &str) -> &str {
    if is_newtab(url) {
        ""
    } else {
        url
    }
}

/// Вид страницы ошибки (ui/error.html, макеты docs/design/Err*.dc.html).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Offline,
    Dns,
    /// Отказ, таймаут и прочие сбои соединения.
    Refused,
    Cert,
    Crash,
    File,
}

impl ErrorKind {
    /// По коду ошибки Chromium (net_error_list.h).
    pub fn for_code(code: i32) -> Self {
        match code {
            -106 => ErrorKind::Offline,                // INTERNET_DISCONNECTED
            -105 | -137 => ErrorKind::Dns,             // NAME_NOT_RESOLVED, NAME_RESOLUTION_FAILED
            -6 | -8 | -10 => ErrorKind::File,          // FILE_NOT_FOUND, FILE_TOO_BIG, ACCESS_DENIED
            -299..=-200 => ErrorKind::Cert,
            _ => ErrorKind::Refused,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Offline => "offline",
            ErrorKind::Dns => "dns",
            ErrorKind::Refused => "refused",
            ErrorKind::Cert => "cert",
            ErrorKind::Crash => "crash",
            ErrorKind::File => "file",
        }
    }
}

/// Сведения о сертификате для страницы ошибки сертификата.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CertInfo {
    pub issuer: String,
    /// Срок действия, секунды Unix.
    pub valid_from: i64,
    pub valid_to: i64,
}

/// Адрес страницы ошибки. Все значения кодируются: они приходят извне.
pub fn error_url(
    kind: ErrorKind,
    failed_url: &str,
    code: &str,
    raw: &str,
    cert: Option<&CertInfo>,
) -> String {
    use crate::urlfix::encode_query as enc;
    let mut url = format!(
        "{}?kind={}&url={}&code={}",
        crate::scheme::ERROR_URL,
        kind.as_str(),
        enc(failed_url),
        enc(code)
    );
    if !raw.is_empty() {
        url.push_str(&format!("&raw={}", enc(raw)));
    }
    if let Some(cert) = cert {
        url.push_str(&format!(
            "&issuer={}&from={}&to={}",
            enc(&cert.issuer),
            cert.valid_from,
            cert.valid_to
        ));
    }
    url
}

/// Это наша страница ошибки.
pub fn is_error_page(url: &str) -> bool {
    url.starts_with(crate::scheme::ERROR_URL)
}

fn data_url(html: &str) -> String {
    format!(
        "data:text/html;charset=utf-8;base64,{}",
        base64(html.as_bytes())
    )
}

fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64("ё".as_bytes()), "0ZE=");
    }

    #[test]
    fn error_kinds_and_urls() {
        assert_eq!(ErrorKind::for_code(-106), ErrorKind::Offline);
        assert_eq!(ErrorKind::for_code(-105), ErrorKind::Dns);
        assert_eq!(ErrorKind::for_code(-201), ErrorKind::Cert);
        assert_eq!(ErrorKind::for_code(-6), ErrorKind::File);
        assert_eq!(ErrorKind::for_code(-102), ErrorKind::Refused);
        let url = error_url(ErrorKind::Dns, "https://a.b/?x=1&y=<2>", "ERR_NAME_NOT_RESOLVED", "", None);
        assert!(is_error_page(&url));
        assert!(url.contains("kind=dns"));
        assert!(url.contains("url=https%3A%2F%2Fa.b%2F%3Fx%3D1%26y%3D%3C2%3E"));
        let cert = CertInfo { issuer: "CA 1".into(), valid_from: 1, valid_to: 2 };
        let url = error_url(ErrorKind::Cert, "https://x", "ERR_CERT_DATE_INVALID", "", Some(&cert));
        assert!(url.ends_with("&issuer=CA+1&from=1&to=2"));
    }

    #[test]
    fn newtab_is_hidden_from_address_bar() {
        assert_eq!(display_url(newtab_url()), "");
        assert_eq!(display_url("rustium://newtab"), "");
        assert_eq!(display_url("https://a.b"), "https://a.b");
        assert!(toolbar_url().starts_with("data:text/html"));
    }
}
