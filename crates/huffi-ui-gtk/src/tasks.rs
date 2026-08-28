use gtk4::glib;

pub fn run_blocking<T, F, G>(work: F, done: G)
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
    G: FnOnce(T) + 'static,
{
    let (tx, rx) = async_channel::unbounded::<T>();
    std::thread::spawn(move || {
        let _ = tx.send_blocking(work());
    });
    glib::spawn_future_local(async move {
        if let Ok(value) = rx.recv().await {
            done(value);
        }
    });
}
