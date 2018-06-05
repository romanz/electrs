use chan;
use chan_signal;
use std::sync::Arc;
use std::time::Duration;

use {daemon, index, store};

pub struct App {
    store: store::DBStore,
    index: index::Index,
    daemon: daemon::Daemon,
}

impl App {
    pub fn new(store: store::DBStore, index: index::Index, daemon: daemon::Daemon) -> Arc<App> {
        Arc::new(App {
            store,
            index,
            daemon,
        })
    }

    pub fn write_store(&self) -> &store::WriteStore {
        &self.store
    }
    pub fn read_store(&self) -> &store::ReadStore {
        &self.store
    }
    pub fn index(&self) -> &index::Index {
        &self.index
    }
    pub fn daemon(&self) -> &daemon::Daemon {
        &self.daemon
    }
}

pub struct Waiter {
    signal: chan::Receiver<chan_signal::Signal>,
    duration: Duration,
}

impl Waiter {
    pub fn new(duration: Duration) -> Waiter {
        let signal = chan_signal::notify(&[chan_signal::Signal::INT]);
        Waiter { signal, duration }
    }
    pub fn wait(&self) -> Option<chan_signal::Signal> {
        let signal = &self.signal;
        let timeout = chan::after(self.duration);
        let result;
        chan_select! {
            signal.recv() -> sig => { result = sig; },
            timeout.recv() => { result = None; },
        }
        result
    }
}
