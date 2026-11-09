//! WebSocket-мост для встроенных DevTools.
//!
//! Фронтенд DevTools (`devtools://devtools/bundled/devtools_app.html`)
//! умеет подключаться к странице только по WebSocket (`?ws=host/path`).
//! Встроенный сервер отладки Chromium для этого не годится: он открывает
//! весь браузер любому локальному процессу. Поэтому поднимаем свой
//! маленький сервер:
//! - слушает только 127.0.0.1 на случайном порту;
//! - путь подключения `/<токен>/<id вкладки>`, токен случайный на запуск,
//!   без него соединение отклоняется;
//! - сообщения CDP из WebSocket отдаём вкладке (`send_dev_tools_message`),
//!   ответы и события вкладки (observer в shell/devtools.rs) — обратно.
//!
//! Каждое соединение живёт в своём потоке. С UI-потоком мост общается
//! через `post_ui` (входящие) и канал (исходящие).

use crate::shell::TabId;
use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{Message, WebSocket};

/// Что делать с сообщением от фронтенда: (вкладка, JSON CDP).
pub type Inbound = Arc<dyn Fn(TabId, String) + Send + Sync>;

pub struct Bridge {
    port: u16,
    token: String,
    /// Открытые сеансы: номер соединения и куда слать сообщения для
    /// фронтенда вкладки.
    sessions: Mutex<HashMap<TabId, (u64, Sender<String>)>>,
    next_session: std::sync::atomic::AtomicU64,
}

static BRIDGE: OnceLock<Option<Arc<Bridge>>> = OnceLock::new();

/// Запустить мост (один раз) или вернуть уже запущенный.
/// `None` — не удалось открыть порт.
pub fn bridge(inbound: impl FnOnce() -> Inbound) -> Option<Arc<Bridge>> {
    BRIDGE
        .get_or_init(|| match Bridge::start(inbound()) {
            Ok(bridge) => Some(bridge),
            Err(err) => {
                eprintln!("rustium: мост DevTools не запустился: {err}");
                None
            }
        })
        .clone()
}

impl Bridge {
    fn start(inbound: Inbound) -> std::io::Result<Arc<Bridge>> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let bridge = Arc::new(Bridge {
            port: listener.local_addr()?.port(),
            token: random_token()?,
            sessions: Mutex::new(HashMap::new()),
            next_session: std::sync::atomic::AtomicU64::new(1),
        });
        let accepting = bridge.clone();
        std::thread::Builder::new()
            .name("devtools-bridge".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let bridge = accepting.clone();
                    let inbound = inbound.clone();
                    let _ = std::thread::Builder::new()
                        .name("devtools-session".into())
                        .spawn(move || bridge.serve(stream, inbound));
                }
            })?;
        Ok(bridge)
    }

    /// Адрес для фронтенда: `devtools_app.html?ws=<это>`.
    pub fn ws_address(&self, tab: TabId) -> String {
        format!("127.0.0.1:{}/{}/{tab}", self.port, self.token)
    }

    /// Сообщение вкладки для её фронтенда DevTools.
    pub fn send(&self, tab: TabId, message: String) {
        if let Some((_, tx)) = self.lock().get(&tab) {
            let _ = tx.send(message);
        }
    }

    /// Закрыть сеанс вкладки (DevTools закрыли).
    pub fn close(&self, tab: TabId) {
        self.lock().remove(&tab);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<TabId, (u64, Sender<String>)>> {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn serve(&self, stream: TcpStream, inbound: Inbound) {
        let mut tab = None;
        // Тип ошибки задаёт трейт tungstenite::handshake::server::Callback.
        #[allow(clippy::result_large_err)]
        let check = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
            match parse_path(request.uri().path(), &self.token) {
                Some(id) => {
                    tab = Some(id);
                    Ok(response)
                }
                None => Err(tungstenite::http::Response::builder()
                    .status(403)
                    .body(Some("forbidden".into()))
                    .expect("статический ответ")),
            }
        };
        let Ok(mut ws) = tungstenite::accept_hdr(stream, check) else {
            return;
        };
        let Some(tab) = tab else { return };
        let (tx, rx) = mpsc::channel();
        let session = self
            .next_session
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Новое подключение того же DevTools вытесняет старое.
        self.lock().insert(tab, (session, tx));
        // Короткий таймаут чтения: в паузах между входящими отправляем исходящие.
        let _ = ws.get_mut().set_read_timeout(Some(Duration::from_millis(10)));
        pump(&mut ws, &rx, tab, &inbound);
        let _ = ws.close(None);
        let _ = ws.flush();
        let mut sessions = self.lock();
        if sessions.get(&tab).is_some_and(|(current, _)| *current == session) {
            sessions.remove(&tab);
        }
    }
}

fn pump(ws: &mut WebSocket<TcpStream>, rx: &Receiver<String>, tab: TabId, inbound: &Inbound) {
    loop {
        match ws.read() {
            Ok(Message::Text(text)) => inbound(tab, text),
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
        loop {
            match rx.try_recv() {
                Ok(message) => {
                    if ws.send(Message::Text(message)).is_err() {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                // Сеанс закрыли из UI (close) или заменили новым.
                Err(TryRecvError::Disconnected) => return,
            }
        }
    }
}

/// `/<токен>/<id>` → id вкладки, если токен совпал.
fn parse_path(path: &str, token: &str) -> Option<TabId> {
    let rest = path.strip_prefix('/')?;
    let (got, id) = rest.split_once('/')?;
    // Сравнение без раннего выхода: по времени ответа токен не подобрать.
    let same = got.len() == token.len()
        && got.bytes().zip(token.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
    if !same {
        return None;
    }
    id.parse().ok()
}

fn random_token() -> std::io::Result<String> {
    let mut bytes = [0u8; 24];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_needs_the_token() {
        assert_eq!(parse_path("/abc/7", "abc"), Some(7));
        assert_eq!(parse_path("/abd/7", "abc"), None);
        assert_eq!(parse_path("/ab/7", "abc"), None);
        assert_eq!(parse_path("/abc/x", "abc"), None);
        assert_eq!(parse_path("/abc", "abc"), None);
        assert_eq!(parse_path("abc/7", "abc"), None);
    }

    #[test]
    fn token_is_random_and_long() {
        let a = random_token().unwrap();
        let b = random_token().unwrap();
        assert_eq!(a.len(), 48);
        assert_ne!(a, b);
    }
}
