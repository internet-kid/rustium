//! Встроенные HTML-страницы: панель браузера и страница новой вкладки.
//!
//! Обе страницы вшиваются в бинарник (`include_str!`) и открываются как
//! `data:`-URL, поэтому никакого локального сервера или файлов рядом
//! с программой не нужно.

use std::sync::OnceLock;

/// Высота панели (вкладки + адресная строка) в логических пикселях.
/// Должна совпадать с `--toolbar-height` в `ui/toolbar.html`.
pub const TOOLBAR_HEIGHT: i32 = 80;

/// Заголовок вкладки, пока страница не прислала свой.
pub const NEW_TAB_TITLE: &str = "New Tab";

const TOOLBAR_HTML: &str = include_str!("../ui/toolbar.html");
const NEWTAB_HTML: &str = include_str!("../ui/newtab.html");
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
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| data_url(&with_fonts(NEWTAB_HTML)))
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
    url == newtab_url()
}

/// URL так, как его видит пользователь: для служебной новой вкладки — пусто.
pub fn display_url(url: &str) -> &str {
    if is_newtab(url) {
        ""
    } else {
        url
    }
}

/// Страница ошибки загрузки (сеть, сертификат, упавший renderer).
/// Все строки экранируются: `failed_url` и текст ошибки приходят извне.
pub fn error_page_url(heading: &str, details: &str, failed_url: &str) -> String {
    let html = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>{title}</title>
<style>
  :root {{ color-scheme: dark; font-family: Geist, system-ui, sans-serif; }}
  body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; background: #16181e; color: #eceef3; }}
  main {{ max-width: 560px; padding: 32px; text-align: center; }}
  h1 {{ font-size: 24px; font-weight: 600; margin: 0 0 12px; }}
  p {{ color: #c9cdd6; line-height: 1.5; margin: 0 0 16px; word-break: break-all; }}
  code {{ font: 12px "Geist Mono", ui-monospace, monospace; color: #9aa0ad; background: #1d2027; padding: 3px 8px; border-radius: 6px; }}
</style></head>
<body><main><h1>{heading}</h1><p>{url}</p><code>{details}</code></main></body></html>"#,
        title = escape_html(heading),
        heading = escape_html(heading),
        url = escape_html(failed_url),
        details = escape_html(details),
    );
    data_url(&html)
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
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
    fn error_page_escapes_input() {
        assert_eq!(
            escape_html(r#"<script>"a"&'b'"#),
            "&lt;script&gt;&quot;a&quot;&amp;&#39;b&#39;"
        );
        assert!(error_page_url("x", "y", "z").starts_with("data:text/html"));
    }

    #[test]
    fn newtab_is_hidden_from_address_bar() {
        assert_eq!(display_url(newtab_url()), "");
        assert_eq!(display_url("https://a.b"), "https://a.b");
        assert!(toolbar_url().starts_with("data:text/html"));
    }
}
