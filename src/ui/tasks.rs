use std::any::Any;
use std::sync::{OnceLock, mpsc};

use gtk4::glib;

struct Job {
    payload: Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>,
    result: async_channel::Sender<Box<dyn Any + Send>>,
}

/// Lazily spawns the single query worker thread and returns a handle to it.
fn worker() -> &'static mpsc::Sender<Job> {
    static WORKER: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::spawn(move || {
            while let Ok(job) = rx.recv() {
                let output = (job.payload)();
                let _ = job.result.send_blocking(output);
            }
        });
        tx
    })
}

/// Run `work` on the shared worker thread and invoke `done` with its result
/// on the main loop.
pub fn run_blocking<T, F, G>(work: F, done: G)
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
    G: FnOnce(T) + 'static,
{
    let (tx, rx) = async_channel::unbounded::<Box<dyn Any + Send>>();
    let job = Job {
        payload: Box::new(move || Box::new(work()) as Box<dyn Any + Send>),
        result: tx,
    };
    let _ = worker().send(job);
    glib::spawn_future_local(async move {
        if let Ok(value) = rx.recv().await
            && let Ok(value) = value.downcast::<T>()
        {
            done(*value);
        }
    });
}
