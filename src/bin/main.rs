extern crate electrs;
extern crate error_chain;

use error_chain::ChainedError;
use std::thread;
use std::time::Duration;

use electrs::{app::{App, Waiter},
              config::Config,
              daemon::Daemon,
              errors::*,
              index::Index,
              query::Query,
              rpc::RPC,
              store::{DBStore, StoreOptions}};

fn run_server(config: &Config) -> Result<()> {
    let daemon = Daemon::new(config.network_type)?;

    let signal = Waiter::new(Duration::from_secs(5));
    let store = DBStore::open(
        &config.db_path,
        StoreOptions {
            // compact manually after the first run has finished successfully
            auto_compact: false,
        },
    );
    let index = Index::load(&store);
    let mut tip = index.update(&store, &daemon)?;
    store.compact_if_needed();
    drop(store); // to be re-opened soon

    let store = DBStore::open(&config.db_path, StoreOptions { auto_compact: true });
    let app = App::new(store, index, daemon);

    let query = Query::new(app.clone());
    let rpc = RPC::start(config.rpc_addr, query.clone());
    while let None = signal.wait() {
        query.update_mempool()?;
        if tip != app.daemon().getbestblockhash()? {
            tip = app.index().update(app.write_store(), app.daemon())?;
        }
        rpc.notify();
    }
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

fn main() {
    let config = Config::from_args();
    for _ in Repeat::new(&config) {
        match run_server(&config) {
            Ok(_) => break,
            Err(e) => eprintln!("{}", e.display_chain()),
        }
    }
}
