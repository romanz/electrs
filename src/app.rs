use std::sync::Arc;

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
