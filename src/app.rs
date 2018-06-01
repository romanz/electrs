use chan;
use chan_signal;
use error_chain::ChainedError;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use config::Config;
use {daemon, index, query, rpc, store};

use errors::*;

pub struct App {
    store: store::DBStore,
    index: index::Index,
    daemon: daemon::Daemon,
}

impl App {
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

struct Waiter {
    signal: chan::Receiver<chan_signal::Signal>,
    duration: Duration,
}

impl Waiter {
    fn new(duration: Duration) -> Waiter {
        let signal = chan_signal::notify(&[chan_signal::Signal::INT]);
        Waiter { signal, duration }
    }
    fn wait(&self) -> Option<chan_signal::Signal> {
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

fn run_server(config: &Config) -> Result<()> {
    let signal = Waiter::new(Duration::from_secs(5));
    let daemon = daemon::Daemon::new(config.network_type)?;
    debug!("{:?}", daemon.getblockchaininfo()?);

    let store = store::DBStore::open(
        config.db_path,
        store::StoreOptions {
            // compact manually after the first run has finished successfully
            auto_compact: false,
        },
    );
    let index = index::Index::load(&store);
    let mut tip = index.update(&store, &daemon)?;
    store.compact_if_needed();
    drop(store); // to be re-opened soon

    let store = store::DBStore::open(config.db_path, store::StoreOptions { auto_compact: true });
    let app = Arc::new(App {
        store,
        index,
        daemon,
    });

    let query = Arc::new(query::Query::new(app.clone()));
    rpc::start(&config.rpc_addr, query.clone());
    while let None = signal.wait() {
        query.update_mempool()?;
        if tip == app.daemon().getbestblockhash()? {
            continue;
        }
        tip = app.index().update(app.write_store(), app.daemon())?;
    }
    info!("closing server");
    Ok(())
}

struct Repeat {
    do_restart: bool,
    iter_count: usize,
}

impl Repeat {
    fn new(config: &Config) -> Repeat {
        Repeat {
            do_restart: config.restart,
            iter_count: 0,
        }
    }
}

impl Iterator for Repeat {
    type Item = ();

    fn next(&mut self) -> Option<()> {
        self.iter_count += 1;
        if self.iter_count == 1 {
            return Some(()); // don't sleep before 1st iteration
        }
        thread::sleep(Duration::from_secs(1));
        if self.do_restart {
            Some(())
        } else {
            None
        }
    }
}

pub fn main() {
    let config = Config::from_args();
    for _ in Repeat::new(&config) {
        match run_server(&config) {
            Ok(_) => break,
            Err(e) => error!("{}", e.display_chain()),
        }
    }
}
