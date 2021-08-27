use anyhow::Result;

pub(crate) fn spawn<F>(name: &'static str, f: F) -> std::thread::JoinHandle<()>
where
    F: 'static + Send + FnOnce() -> Result<()>,
{
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            if let Err(e) = f() {
                warn!("{} thread failed: {}", name, e);
            }
        })
        .expect("failed to spawn a thread")
}
