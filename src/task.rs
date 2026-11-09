//! Постановка замыканий в очередь UI-потока CEF.
//!
//! Почти все изменения дерева views (вкладки, окно) выполняются через
//! `post_ui`, а не прямо внутри колбэков CEF. Так проще избежать
//! повторного входа в наши обработчики и взаимных блокировок.

use cef::*;
use std::sync::{Arc, Mutex};

type BoxedFn = Box<dyn FnOnce() + Send + 'static>;

wrap_task! {
    struct ClosureTask {
        func: Arc<Mutex<Option<BoxedFn>>>,
    }

    impl Task {
        fn execute(&self) {
            let func = self.func.lock().ok().and_then(|mut f| f.take());
            if let Some(func) = func {
                func();
            }
        }
    }
}

fn make_task<F: FnOnce() + Send + 'static>(f: F) -> Task {
    ClosureTask::new(Arc::new(Mutex::new(Some(Box::new(f) as BoxedFn))))
}

/// Выполнить `f` на UI-потоке браузерного процесса (асинхронно).
pub fn post_ui<F: FnOnce() + Send + 'static>(f: F) {
    let mut task = make_task(f);
    post_task(ThreadId::UI, Some(&mut task));
}

/// То же, но с задержкой.
pub fn post_ui_delayed<F: FnOnce() + Send + 'static>(delay_ms: i64, f: F) {
    let mut task = make_task(f);
    post_delayed_task(ThreadId::UI, Some(&mut task), delay_ms);
}
