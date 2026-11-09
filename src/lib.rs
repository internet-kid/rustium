//! Rustium — прототип браузера на Rust + CEF (Chromium Embedded Framework).
//!
//! Устройство:
//! - окно и раскладка — CEF Views (`views.rs`);
//! - панель с вкладками и адресной строкой — HTML/JS (`ui/toolbar.html`),
//!   отдельный BrowserView сверху окна;
//! - каждая вкладка — свой BrowserView (Chromium), видна только активная;
//! - панель общается с Rust через `cefQuery` (`ipc.rs`), Rust с панелью —
//!   через `execute_java_script` (`shell.rs`).

pub mod app;
pub mod client;
pub mod downloads;
pub mod ipc;
pub mod shell;
pub mod shortcuts;
pub mod task;
pub mod ui;
pub mod urlfix;
pub mod views;

#[cfg(target_os = "macos")]
pub mod mac;

pub use app::{run_helper, run_main};
