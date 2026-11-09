//! Загрузки: модель для панели, имена файлов и действия с файлами.
//!
//! Модуль не зависит от CEF (кроме мест, явно помеченных), поэтому
//! логика имён и состояний покрыта обычными тестами.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Состояние загрузки для панели (`downloads` в `UiState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Progress,
    Paused,
    Complete,
    Failed,
    Canceled,
}

/// Одна загрузка в списке.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Download {
    pub id: u32,
    pub name: String,
    #[serde(skip)]
    pub path: PathBuf,
    #[serde(skip)]
    pub url: String,
    pub received: i64,
    /// -1, если сервер не сообщил размер.
    pub total: i64,
    /// Байт в секунду.
    pub speed: i64,
    pub status: Status,
    /// Причина ошибки для `Failed`.
    pub error: Option<String>,
}

/// Снимок `DownloadItem` из CEF, чтобы не держать объект CEF под блокировкой.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub id: u32,
    pub path: String,
    pub url: String,
    pub suggested_name: String,
    pub received: i64,
    pub total: i64,
    pub speed: i64,
    pub in_progress: bool,
    pub paused: bool,
    pub complete: bool,
    pub canceled: bool,
    pub interrupted: bool,
    pub interrupt_reason: u32,
}

impl Snapshot {
    pub fn status(&self) -> Status {
        if self.complete {
            Status::Complete
        } else if self.canceled {
            Status::Canceled
        } else if self.interrupted {
            Status::Failed
        } else if self.paused {
            Status::Paused
        } else {
            Status::Progress
        }
    }
}

impl Download {
    pub fn from_snapshot(s: &Snapshot) -> Self {
        let mut d = Download {
            id: s.id,
            name: String::new(),
            path: PathBuf::new(),
            url: String::new(),
            received: 0,
            total: -1,
            speed: 0,
            status: Status::Progress,
            error: None,
        };
        d.apply(s);
        d
    }

    /// Обновить по свежему снимку. Пустые поля снимка не затирают известные.
    pub fn apply(&mut self, s: &Snapshot) {
        if !s.path.is_empty() {
            self.path = PathBuf::from(&s.path);
        }
        if !s.url.is_empty() {
            self.url = s.url.clone();
        }
        let from_path = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty());
        if let Some(name) = from_path {
            self.name = name;
        } else if self.name.is_empty() {
            self.name = sanitize_file_name(&s.suggested_name);
        }
        self.received = s.received;
        self.total = s.total;
        self.speed = s.speed;
        // CEF присылает «отменена» и после отмены пользователем, и при выходе.
        self.status = s.status();
        self.error = (self.status == Status::Failed).then(|| interrupt_text(s.interrupt_reason));
    }

    pub fn is_active(&self) -> bool {
        matches!(self.status, Status::Progress | Status::Paused)
    }
}

/// Значок кнопки загрузок в панели.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Badge {
    /// Загрузок нет: кнопка скрыта.
    Hidden,
    /// Обычная кнопка.
    Idle,
    /// Кольцо прогресса.
    Progress,
    /// Зелёная отметка, пока список не открыли.
    Complete,
    /// Красная отметка, пока список не открыли.
    Failed,
}

/// Значок и общий прогресс (0..=1) по списку загрузок.
/// `seen` — пользователь открывал список после последнего завершения.
pub fn badge(list: &[Download], seen: bool) -> (Badge, f64) {
    if list.is_empty() {
        return (Badge::Hidden, 0.0);
    }
    let active: Vec<&Download> = list.iter().filter(|d| d.is_active()).collect();
    if !active.is_empty() {
        let known = active.iter().all(|d| d.total > 0);
        let progress = if known {
            let received: i64 = active.iter().map(|d| d.received).sum();
            let total: i64 = active.iter().map(|d| d.total).sum();
            (received as f64 / total as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        return (Badge::Progress, progress);
    }
    if seen {
        return (Badge::Idle, 0.0);
    }
    // Последняя завершившаяся загрузка в списке — первая (новые сверху).
    match list.first().map(|d| d.status) {
        Some(Status::Failed) => (Badge::Failed, 0.0),
        Some(Status::Complete) => (Badge::Complete, 0.0),
        _ => (Badge::Idle, 0.0),
    }
}

/// Текст ошибки по `cef_download_interrupt_reason_t` (коды Chromium).
pub fn interrupt_text(reason: u32) -> String {
    match reason {
        3 => "disk is full",
        2 => "permission denied",
        5 | 6 => "file name or size not allowed",
        1..=19 => "couldn't save the file",
        20..=29 => "network error",
        34 | 36 => "access denied by the server",
        30..=39 => "server error",
        41 => "browser was closed",
        _ => "download failed",
    }
    .to_string()
}

/// Имя файла без путей и управляющих символов.
pub fn sanitize_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() || c == ':' { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_string();
    if cleaned.is_empty() {
        "download".to_string()
    } else {
        cleaned
    }
}

/// Свободное имя в папке: `file.zip`, `file (1).zip`, `file (2).zip`…
pub fn unique_path(dir: &Path, name: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let name = sanitize_file_name(name);
    let first = dir.join(&name);
    if !exists(&first) {
        return first;
    }
    // Двойные расширения вроде .tar.gz сохраняем целиком.
    let (stem, ext) = split_extension(&name);
    (1..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|p| !exists(p))
        .expect("бесконечный итератор")
}

fn split_extension(name: &str) -> (&str, &str) {
    for double in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"] {
        if name.len() > double.len() && name.to_ascii_lowercase().ends_with(double) {
            let at = name.len() - double.len();
            return (&name[..at], &name[at..]);
        }
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Папка загрузок: ~/Downloads (или XDG_DOWNLOAD_DIR на Linux).
/// RUSTIUM_DOWNLOADS_DIR переопределяет её (удобно для проверок).
pub fn downloads_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RUSTIUM_DOWNLOADS_DIR") {
        return PathBuf::from(dir);
    }
    if cfg!(target_os = "linux") {
        if let Some(dir) = std::env::var_os("XDG_DOWNLOAD_DIR") {
            return PathBuf::from(dir);
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join("Downloads")
}

/// Открыть файл программой по умолчанию.
pub fn open_file(path: &Path) {
    let program = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    spawn(program, &[path.as_os_str()]);
}

/// Показать файл в Finder (macOS) или открыть его папку (Linux).
pub fn reveal_file(path: &Path) {
    if cfg!(target_os = "macos") {
        spawn("open", &["-R".as_ref(), path.as_os_str()]);
    } else if let Some(dir) = path.parent() {
        spawn("xdg-open", &[dir.as_os_str()]);
    }
}

fn spawn(program: &str, args: &[&std::ffi::OsStr]) {
    if let Err(err) = std::process::Command::new(program).args(args).spawn() {
        eprintln!("rustium: не удалось запустить {program}: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn unique_names() {
        let taken: HashSet<PathBuf> = ["/d/a.zip", "/d/a (1).zip", "/d/x.tar.gz", "/d/noext"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let exists = |p: &Path| taken.contains(p);
        let dir = Path::new("/d");
        assert_eq!(unique_path(dir, "b.zip", exists), PathBuf::from("/d/b.zip"));
        assert_eq!(unique_path(dir, "a.zip", exists), PathBuf::from("/d/a (2).zip"));
        assert_eq!(unique_path(dir, "x.tar.gz", exists), PathBuf::from("/d/x (1).tar.gz"));
        assert_eq!(unique_path(dir, "noext", exists), PathBuf::from("/d/noext (1)"));
    }

    #[test]
    fn names_are_sanitized() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("..\\evil.exe"), "evil.exe");
        assert_eq!(sanitize_file_name(".hidden"), "hidden");
        assert_eq!(sanitize_file_name("a:b\n.txt"), "a_b_.txt");
        assert_eq!(sanitize_file_name(""), "download");
    }

    fn dl(id: u32, status: Status, received: i64, total: i64) -> Download {
        Download {
            id,
            name: format!("f{id}"),
            path: PathBuf::new(),
            url: String::new(),
            received,
            total,
            speed: 0,
            status,
            error: None,
        }
    }

    #[test]
    fn badge_states() {
        assert_eq!(badge(&[], false).0, Badge::Hidden);
        let (b, p) = badge(&[dl(1, Status::Progress, 25, 100), dl(2, Status::Complete, 5, 5)], false);
        assert_eq!(b, Badge::Progress);
        assert!((p - 0.25).abs() < 1e-9);
        assert_eq!(badge(&[dl(2, Status::Complete, 5, 5)], false).0, Badge::Complete);
        assert_eq!(badge(&[dl(2, Status::Failed, 1, 5)], false).0, Badge::Failed);
        assert_eq!(badge(&[dl(2, Status::Failed, 1, 5)], true).0, Badge::Idle);
    }

    #[test]
    fn snapshot_status_and_name() {
        let s = Snapshot {
            id: 7,
            path: "/Users/a/Downloads/file.pdf".into(),
            suggested_name: "ignored.pdf".into(),
            interrupted: true,
            interrupt_reason: 22,
            ..Default::default()
        };
        let d = Download::from_snapshot(&s);
        assert_eq!(d.name, "file.pdf");
        assert_eq!(d.status, Status::Failed);
        assert_eq!(d.error.as_deref(), Some("network error"));
    }
}
