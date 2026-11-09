//! Загрузки: события `DownloadHandler` (client.rs) и действия из списка.

use super::{cef_str, Shell};
use crate::downloads::{self, Download, Snapshot, Status};
use crate::ipc::{DownloadAction, PopupKind};
use cef::*;

impl Shell {
    /// Загрузка началась (`on_before_download`, путь уже выбран).
    pub(crate) fn download_started(&self, snapshot: Snapshot) {
        let window_active = {
            let mut s = self.lock();
            match s.downloads.iter_mut().find(|d| d.id == snapshot.id) {
                Some(d) => d.apply(&snapshot),
                None => s.downloads.insert(0, Download::from_snapshot(&snapshot)),
            }
            s.downloads_seen = false;
            s.window_active
        };
        self.push_state();
        // Как в Chrome: показываем список, чтобы было видно, что пошла загрузка.
        if window_active {
            self.run_in_toolbar("window.rustium && window.rustium.openDownloads();");
        }
    }

    /// Прогресс, пауза, завершение, ошибка (`on_download_updated`).
    pub(crate) fn download_updated(
        &self,
        snapshot: Snapshot,
        callback: Option<DownloadItemCallback>,
    ) {
        let stale_callback = {
            let mut s = self.lock();
            if s.removed_downloads.contains(&snapshot.id) {
                return;
            }
            let popup_shows_list = s.popup == Some(PopupKind::Downloads);
            let index = match s.downloads.iter().position(|d| d.id == snapshot.id) {
                Some(i) => i,
                None => {
                    // Загрузка, о которой мы не знали (например, начатая
                    // до появления обработчика): показываем и её.
                    s.downloads.insert(0, Download::from_snapshot(&snapshot));
                    0
                }
            };
            let was_active = s.downloads[index].is_active();
            s.downloads[index].apply(&snapshot);
            let now_active = s.downloads[index].is_active();
            if was_active && !now_active {
                s.downloads_seen = popup_shows_list;
            }
            match callback {
                Some(cb) if now_active => s.download_callbacks.insert(snapshot.id, cb),
                _ => s.download_callbacks.remove(&snapshot.id),
            }
        };
        // Объекты CEF отпускаем вне блокировки.
        drop(stale_callback);
        self.push_state();
    }

    pub(super) fn download_action(&self, id: u32, action: DownloadAction) {
        let (download, callback) = {
            let s = self.lock();
            (
                s.downloads.iter().find(|d| d.id == id).cloned(),
                s.download_callbacks.get(&id).cloned(),
            )
        };
        let Some(download) = download else { return };
        match action {
            DownloadAction::Pause => {
                if let Some(cb) = callback {
                    cb.pause();
                }
            }
            DownloadAction::Resume => {
                if let Some(cb) = callback {
                    cb.resume();
                }
            }
            DownloadAction::Cancel => {
                if let Some(cb) = callback {
                    cb.cancel();
                }
            }
            DownloadAction::Open => {
                if download.status == Status::Complete && download.path.exists() {
                    downloads::open_file(&download.path);
                    self.close_popup(false);
                }
            }
            DownloadAction::Show => {
                if download.path.exists() {
                    downloads::reveal_file(&download.path);
                }
            }
            DownloadAction::Retry => {
                self.remove_download(id, callback);
                if !download.url.is_empty() {
                    self.with_active_browser(|b| {
                        if let Some(host) = b.host() {
                            host.start_download(Some(&cef_str(&download.url)));
                        }
                    });
                }
            }
            DownloadAction::Remove => self.remove_download(id, callback),
        }
    }

    fn remove_download(&self, id: u32, callback: Option<DownloadItemCallback>) {
        // Убираем из списка до отмены: on_download_updated с «отменена»
        // иначе вернёт строку обратно.
        let (removed, empty) = {
            let mut s = self.lock();
            s.downloads.retain(|d| d.id != id);
            s.removed_downloads.insert(id);
            let removed = s.download_callbacks.remove(&id);
            (removed, s.downloads.is_empty())
        };
        if let Some(cb) = callback {
            cb.cancel();
        }
        drop(removed);
        if empty {
            self.close_popup(false);
        }
        self.push_state();
    }
}
