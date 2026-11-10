//! Особенности macOS.
//!
//! 1. Фреймворк CEF загружается динамически из бандла (.app).
//! 2. NSApp должен быть подклассом NSApplication с протоколом
//!    CefAppProtocol: Chromium через него узнаёт, обрабатывается ли
//!    сейчас событие (`isHandlingSendEvent`).
//! 3. «Завершить» из Dock/меню (`terminate:`) закрывает окно штатно,
//!    чтобы CEF успел корректно остановиться.

use cef::application_mac::{CefAppProtocol, CrAppControlProtocol, CrAppProtocol};
use cef::library_loader::LibraryLoader;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{define_class, msg_send, ClassType, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSEvent};
use objc2_foundation::NSObjectProtocol;
use std::sync::atomic::{AtomicBool, Ordering};

const FRAMEWORK: &str = "Chromium Embedded Framework.framework/Chromium Embedded Framework";

/// Загружает «Chromium Embedded Framework.framework» из бандла.
/// Библиотека выгружается, когда возвращённый загрузчик уничтожается.
pub fn load_cef_framework(helper: bool) -> Option<LibraryLoader> {
    let exe = std::env::current_exe().ok()?;
    // Главный: Rustium.app/Contents/MacOS/rustium -> ../Frameworks
    // Helper:  .../Frameworks/rustium Helper.app/Contents/MacOS/... -> ../../..
    let relative = if helper { "../../.." } else { "../Frameworks" };
    let framework = exe.parent()?.join(relative).join(FRAMEWORK);
    if !framework.exists() {
        eprintln!("rustium: не найден {}", framework.display());
        eprintln!("На macOS запускайте приложение из бандла:");
        eprintln!("  cargo run --bin bundle && open target/bundle/rustium.app");
        return None;
    }
    let loader = LibraryLoader::new(&exe, helper);
    if !loader.load() {
        eprintln!("rustium: не удалось загрузить {}", framework.display());
        return None;
    }
    Some(loader)
}

static HANDLING_SEND_EVENT: AtomicBool = AtomicBool::new(false);

define_class!(
    // SAFETY: NSApplication можно наследовать; Drop не реализуем,
    // состояние хранится в статической переменной (NSApp один).
    #[unsafe(super(NSApplication))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustiumApplication"]
    struct RustiumApplication;

    impl RustiumApplication {
        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            let previous = HANDLING_SEND_EVENT.swap(true, Ordering::Relaxed);
            let _: () = unsafe { msg_send![super(self), sendEvent: event] };
            HANDLING_SEND_EVENT.store(previous, Ordering::Relaxed);
        }

        #[unsafe(method(terminate:))]
        fn terminate(&self, _sender: Option<&AnyObject>) {
            crate::shell::request_quit();
        }

        // Chromium вызывает это, когда пользователь ответил «Остаться»
        // на вопрос «Покинуть сайт?» во время выхода. Без метода
        // приложение падает с unrecognized selector.
        #[unsafe(method(cancelTerminate:))]
        fn cancel_terminate(&self, _sender: Option<&AnyObject>) {
            crate::shell::close_cancelled();
        }
    }

    unsafe impl NSObjectProtocol for RustiumApplication {}

    unsafe impl CrAppProtocol for RustiumApplication {
        #[unsafe(method(isHandlingSendEvent))]
        fn is_handling_send_event(&self) -> Bool {
            Bool::new(HANDLING_SEND_EVENT.load(Ordering::Relaxed))
        }
    }

    unsafe impl CrAppControlProtocol for RustiumApplication {
        #[unsafe(method(setHandlingSendEvent:))]
        fn set_handling_send_event(&self, handling: Bool) {
            HANDLING_SEND_EVENT.store(handling.as_bool(), Ordering::Relaxed);
        }
    }

    unsafe impl CefAppProtocol for RustiumApplication {}
);

/// Делает NSApp экземпляром RustiumApplication. Вызывать до инициализации
/// CEF и после загрузки фреймворка (в нём объявлены протоколы).
pub fn init_application() {
    let _app: Retained<NSApplication> =
        unsafe { msg_send![RustiumApplication::class(), sharedApplication] };
}

/// Скруглить всплывающее окно (меню, загрузки) и включить системную тень.
///
/// Overlay в CEF Views на macOS — дочернее NSWindow. Браузер в нём не умеет
/// быть прозрачным, поэтому прозрачными делаем само окно, а содержимое
/// обрезаем слоем со скруглёнными углами. Окно ищем среди дочерних окон
/// по размеру. Возвращает `false`, если такого окна нет.
pub fn style_popup_window(width: f64, height: f64, radius: f64) -> bool {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSRect;

    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let windows: *mut AnyObject = msg_send![app, windows];
        let count: usize = msg_send![windows, count];
        let mut found = false;
        for i in 0..count {
            let window: *mut AnyObject = msg_send![windows, objectAtIndex: i];
            let parent: *mut AnyObject = msg_send![window, parentWindow];
            if parent.is_null() {
                continue;
            }
            let frame: NSRect = msg_send![window, frame];
            if (frame.size.width - width).abs() > 1.0 || (frame.size.height - height).abs() > 1.0 {
                continue;
            }
            let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
            let _: () = msg_send![window, setOpaque: Bool::NO];
            let _: () = msg_send![window, setBackgroundColor: clear];
            let content: *mut AnyObject = msg_send![window, contentView];
            if !content.is_null() {
                let _: () = msg_send![content, setWantsLayer: Bool::YES];
                let layer: *mut AnyObject = msg_send![content, layer];
                if !layer.is_null() {
                    let _: () = msg_send![layer, setCornerRadius: radius];
                    let _: () = msg_send![layer, setMasksToBounds: Bool::YES];
                }
            }
            let _: () = msg_send![window, setHasShadow: Bool::YES];
            let _: () = msg_send![window, invalidateShadow];
            found = true;
        }
        if !found && std::env::var_os("RUSTIUM_DEBUG_POPUP").is_some() {
            eprintln!("[popup] дочернее окно {width}x{height} не найдено среди {count} окон");
        }
        found
    }
}

// -------------------------------------------------------------------------
// Браузер по умолчанию и ссылки из других приложений
// -------------------------------------------------------------------------

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn LSCopyDefaultHandlerForURLScheme(scheme: *const AnyObject) -> *mut AnyObject;
    fn LSSetDefaultHandlerForURLScheme(scheme: *const AnyObject, bundle_id: *const AnyObject) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(object: *const AnyObject);
}

fn bundle_id() -> Option<Retained<objc2_foundation::NSString>> {
    objc2_foundation::NSBundle::mainBundle().bundleIdentifier()
}

/// Rustium открывает http-ссылки? `None` — запущен не из бандла.
pub fn is_default_browser() -> Option<bool> {
    use objc2_foundation::NSString;
    let ours = bundle_id()?;
    let scheme = NSString::from_str("http");
    // SAFETY: NSString и CFString — один тип (toll-free bridging);
    // функция Copy возвращает объект, который мы освобождаем.
    unsafe {
        let handler = LSCopyDefaultHandlerForURLScheme(Retained::as_ptr(&scheme).cast());
        if handler.is_null() {
            return Some(false);
        }
        let handler_str = &*(handler as *const NSString);
        let same = handler_str.to_string().eq_ignore_ascii_case(&ours.to_string());
        CFRelease(handler);
        Some(same)
    }
}

/// Попросить систему открывать http(s) в Rustium. macOS сама спросит
/// пользователя; ответ придёт позже, поэтому результат не возвращаем.
pub fn make_default_browser() {
    use objc2_foundation::NSString;
    let Some(ours) = bundle_id() else { return };
    for scheme in ["http", "https"] {
        let scheme = NSString::from_str(scheme);
        // SAFETY: см. is_default_browser.
        let status = unsafe {
            LSSetDefaultHandlerForURLScheme(
                Retained::as_ptr(&scheme).cast(),
                Retained::as_ptr(&ours).cast(),
            )
        };
        if status != 0 {
            eprintln!("rustium: LSSetDefaultHandlerForURLScheme: {status}");
        }
    }
}

/// 'GURL' — Apple Event «открыть URL» (kInternetEventClass / kAEGetURL).
const GURL: u32 = u32::from_be_bytes(*b"GURL");
/// '----' — прямой параметр события (keyDirectObject).
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

define_class!(
    // SAFETY: обычный NSObject без состояния.
    #[unsafe(super(objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustiumURLHandler"]
    struct UrlHandler;

    impl UrlHandler {
        #[unsafe(method(handleGetURL:withReplyEvent:))]
        fn handle_get_url(&self, event: &AnyObject, _reply: &AnyObject) {
            let url: Option<String> = unsafe {
                let param: *mut AnyObject = msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT];
                if param.is_null() {
                    None
                } else {
                    let s: *mut objc2_foundation::NSString = msg_send![param, stringValue];
                    s.as_ref().map(|s| s.to_string())
                }
            };
            if let Some(url) = url {
                crate::shell::open_external(url);
            }
        }
    }

    unsafe impl NSObjectProtocol for UrlHandler {}
);

/// Принимать ссылки, которые открывают в Rustium другие приложения.
/// Вызывать до запуска цикла сообщений: ссылка, с которой нас запустили,
/// приходит первым же событием.
pub fn install_url_handler() {
    use objc2::MainThreadMarker;
    let Some(mtm) = MainThreadMarker::new() else { return };
    let handler: Retained<UrlHandler> = unsafe { msg_send![UrlHandler::alloc(mtm), init] };
    unsafe {
        let manager: *mut AnyObject =
            msg_send![objc2::class!(NSAppleEventManager), sharedAppleEventManager];
        let _: () = msg_send![
            manager,
            setEventHandler: &*handler,
            andSelector: objc2::sel!(handleGetURL:withReplyEvent:),
            forEventClass: GURL,
            andEventID: GURL
        ];
    }
    // Менеджер не держит ссылку на обработчик: живёт до выхода.
    std::mem::forget(handler);
}
