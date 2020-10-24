use anyhow::Result;
use async_std::task;
use futures::{channel::mpsc, future::Future};

pub type Sender<T> = mpsc::UnboundedSender<T>;
pub type Receiver<T> = mpsc::UnboundedReceiver<T>;

pub use mpsc::unbounded;

pub fn spawn<F>(name: &'static str, fut: F) -> task::JoinHandle<()>
where
    F: Future<Output = Result<()>> + Send + 'static,
{
    task::spawn(async move {
        if let Err(e) = fut.await {
            error!("{} failed: {:?}", name, e)
        }
    })
}
