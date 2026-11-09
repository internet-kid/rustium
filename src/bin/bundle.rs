//! Сборка запускаемого бандла:
//!
//!   cargo run --bin bundle                 # debug-сборка
//!   cargo run --bin bundle -- --release    # release (Linux)
//!
//! Linux: в target/bundle/ кладутся rustium и файлы CEF (libcef.so,
//!        ресурсы, locales). Запуск: ./target/bundle/rustium
//! macOS: собирается target/bundle/rustium.app с фреймворком CEF и
//!        helper-приложениями. Запуск: open target/bundle/rustium.app

use std::path::PathBuf;

fn main() {
    let mut release = false;
    let mut output = PathBuf::from("target/bundle");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--release" => release = true,
            "-o" | "--output" => output = args.next().map(PathBuf::from).unwrap_or(output),
            "-h" | "--help" => {
                println!("cargo run --bin bundle [-- --release] [-- -o <dir>]");
                return;
            }
            other => {
                eprintln!("неизвестный аргумент: {other}");
                std::process::exit(2);
            }
        }
    }
    if let Err(err) = std::fs::create_dir_all(&output) {
        eprintln!("не удалось создать {}: {err}", output.display());
        std::process::exit(1);
    }
    match build(&output, release) {
        Ok(path) => println!("Готово: {}", path.display()),
        Err(err) => {
            eprintln!("Сборка бандла не удалась: {err}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
fn build(output: &std::path::Path, release: bool) -> Result<PathBuf, String> {
    cef::build_util::linux::build_bundle(output, "rustium", release).map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
fn build(output: &std::path::Path, release: bool) -> Result<PathBuf, String> {
    use cef::build_util::mac::{build_bundle, BundleInfo};
    if release {
        eprintln!("Примечание: cef::build_util на macOS собирает debug-версию.");
    }
    let info = BundleInfo::new(
        "rustium",
        "dev.rustium.browser",
        "Rustium",
        "English",
        semver::Version::new(0, 1, 0),
    );
    build_bundle(output, "rustium", info).map_err(|e| e.to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn build(_output: &std::path::Path, _release: bool) -> Result<PathBuf, String> {
    Err("бандл для этой ОС не настроен (см. cef::build_util::win)".into())
}
