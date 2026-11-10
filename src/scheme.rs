//! Схема `rustium://` — служебные страницы браузера.
//!
//! - `rustium://newtab/` — новая вкладка;
//! - `rustium://error/?kind=…&url=…` — страницы ошибок (ui/error.html);
//! - `rustium://settings/<раздел>` — настройки (ui/settings.html);
//! - `rustium://assets/…` — общие файлы: шрифт, стили.
//!
//! Схема регистрируется как STANDARD (обычные URL с хостом и путём),
//! SECURE (безопасный контекст: работает clipboard и т.п.) и LOCAL: как и
//! `file://`, её нельзя открыть или встроить со страниц сайтов — только из
//! адресной строки или самим браузером. Файлы вшиты в бинарник.

use cef::*;
use std::os::raw::c_int;
use std::sync::{Arc, Mutex};

pub const SCHEME: &str = "rustium";
pub const NEWTAB_URL: &str = "rustium://newtab/";
pub const ERROR_URL: &str = "rustium://error/";
pub const SETTINGS_URL: &str = "rustium://settings/";

/// Метка в CSS страниц, вместо которой подставляется `@font-face` с Geist.
const FONT_MARKER: &str = "/*@font-geist*/";
const FONT_FACE: &str = "@font-face { font-family: Geist; font-weight: 100 900; \
     font-display: block; src: url(rustium://assets/fonts/Geist-Variable.ttf) format('truetype'); }";

const NEWTAB_HTML: &str = include_str!("../ui/newtab.html");
const ERROR_HTML: &str = include_str!("../ui/error.html");
const SETTINGS_HTML: &str = include_str!("../ui/settings.html");
const GEIST: &[u8] = include_bytes!("../ui/fonts/Geist-Variable.ttf");

/// Ресурс по URL: (содержимое, MIME-тип). `None` — такого нет.
pub fn resource(url: &str) -> Option<(Vec<u8>, &'static str)> {
    let rest = url.strip_prefix("rustium://")?;
    // Хост и путь без query и fragment.
    let path = rest.split(['?', '#']).next().unwrap_or("");
    let path = path.trim_end_matches('/');
    let engine = crate::prefs::get().search_engine;
    let prefix = engine.search_prefix();
    let action = prefix.split('?').next().unwrap_or(prefix);
    let html = |s: &str| {
        let page = s
            .replacen(FONT_MARKER, FONT_FACE, 1)
            .replace("{{search_action}}", action)
            .replace("{{search_name}}", engine.label())
            .replace("{{search_prefix}}", prefix);
        (page.into_bytes(), "text/html")
    };
    // rustium://settings/<раздел> — одна страница, раздел выбирает она сама.
    if path == "settings" || path.starts_with("settings/") {
        return Some(html(SETTINGS_HTML));
    }
    Some(match path {
        "newtab" => html(NEWTAB_HTML),
        "error" => html(ERROR_HTML),
        "assets/fonts/Geist-Variable.ttf" => (GEIST.to_vec(), "font/ttf"),
        _ => return None,
    })
}

/// Зарегистрировать схему. Вызывается во всех процессах
/// (`App::on_register_custom_schemes`).
pub fn register_custom_scheme(registrar: &mut SchemeRegistrar) {
    let options = SchemeOptions::STANDARD.get_raw()
        | SchemeOptions::LOCAL.get_raw()
        | SchemeOptions::SECURE.get_raw()
        | SchemeOptions::CORS_ENABLED.get_raw()
        | SchemeOptions::FETCH_ENABLED.get_raw();
    registrar.add_custom_scheme(Some(&CefString::from(SCHEME)), options as c_int);
}

/// Подключить обработчик схемы. Вызывается в браузерном процессе после
/// инициализации CEF.
pub fn register_handler_factory() {
    let mut factory = RustiumSchemeFactory::new();
    register_scheme_handler_factory(
        Some(&CefString::from(SCHEME)),
        None,
        Some(&mut factory),
    );
}

wrap_scheme_handler_factory! {
    struct RustiumSchemeFactory {}

    impl SchemeHandlerFactory {
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let url = request
                .map(|r| CefString::from(&r.url()).to_string())
                .unwrap_or_default();
            let (body, mime, status) = match resource(&url) {
                Some((body, mime)) => (body, mime, 200),
                None => (b"Not found".to_vec(), "text/plain", 404),
            };
            Some(MemoryResourceHandler::new(
                Arc::new(Mutex::new(Body { data: body, offset: 0 })),
                mime,
                status,
            ))
        }
    }
}

struct Body {
    data: Vec<u8>,
    offset: usize,
}

wrap_resource_handler! {
    struct MemoryResourceHandler {
        body: Arc<Mutex<Body>>,
        mime: &'static str,
        status: c_int,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut Callback>,
        ) -> ::std::os::raw::c_int {
            // Данные уже в памяти: отвечаем сразу.
            if let Some(handle) = handle_request {
                *handle = 1;
            }
            1
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            if let Some(response) = response {
                response.set_status(self.status);
                response.set_mime_type(Some(&CefString::from(self.mime)));
                if self.mime.starts_with("text/") {
                    response.set_charset(Some(&CefString::from("utf-8")));
                }
                response.set_header_by_name(
                    Some(&CefString::from("Cache-Control")),
                    Some(&CefString::from("no-store")),
                    1,
                );
            }
            if let Some(length) = response_length {
                *length = self.body.lock().map(|b| b.data.len() as i64).unwrap_or(0);
            }
        }

        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: ::std::os::raw::c_int,
            bytes_read: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> ::std::os::raw::c_int {
            let Ok(mut body) = self.body.lock() else { return 0 };
            let left = body.data.len() - body.offset;
            let n = left.min(bytes_to_read.max(0) as usize);
            if n > 0 && !data_out.is_null() {
                // SAFETY: CEF даёт буфер размером bytes_to_read; n не больше.
                unsafe {
                    std::ptr::copy_nonoverlapping(body.data.as_ptr().add(body.offset), data_out, n);
                }
                body.offset += n;
            }
            if let Some(read) = bytes_read {
                *read = n as c_int;
            }
            // 0 — данные кончились.
            (n > 0) as c_int
        }

        fn cancel(&self) {}
    }
}

/// `Basetime` CEF (микросекунды с 1601-01-01 UTC) → секунды Unix.
pub fn basetime_to_unix(micros_since_1601: i64) -> i64 {
    const EPOCH_DIFF_SECS: i64 = 11_644_473_600;
    micros_since_1601 / 1_000_000 - EPOCH_DIFF_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_known_paths() {
        assert_eq!(resource("rustium://newtab/").unwrap().1, "text/html");
        assert_eq!(resource("rustium://newtab").unwrap().1, "text/html");
        assert_eq!(resource("rustium://error/?kind=dns&url=x").unwrap().1, "text/html");
        assert_eq!(resource("rustium://assets/fonts/Geist-Variable.ttf").unwrap().1, "font/ttf");
        assert_eq!(resource("rustium://settings/privacy").unwrap().1, "text/html");
        assert!(resource("rustium://settingsx/").is_none());
        assert!(resource("rustium://nope/").is_none());
        assert!(resource("https://newtab/").is_none());
        let page = String::from_utf8(resource("rustium://newtab/").unwrap().0).unwrap();
        assert!(page.contains("rustium://assets/fonts/Geist-Variable.ttf"));
        assert!(page.contains("Search with DuckDuckGo"));
        assert!(!page.contains("{{"));
    }

    #[test]
    fn basetime() {
        // 2026-01-01T00:00:00Z
        assert_eq!(basetime_to_unix((1_767_225_600 + 11_644_473_600) * 1_000_000), 1_767_225_600);
    }
}
