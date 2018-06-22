extern crate electrs;
extern crate error_chain;

use error_chain::ChainedError;
use std::time::Duration;

use electrs::{
    app::App, config::Config, daemon::Daemon, errors::*, index::Index, metrics::Metrics,
    query::Query, rpc::RPC, signal::Waiter, store::{DBStore, StoreOptions},
};

fn run_server(config: &Config) -> Result<()> {
    let signal = Waiter::new();
    let metrics = Metrics::new(config.monitoring_addr);
    let daemon = Daemon::new(config.network_type, &metrics)?;
    let store = DBStore::open(&config.db_path, StoreOptions { bulk_import: true });
    let index = Index::load(&store, &daemon, &metrics)?;

    metrics.start();
    let mut tip = index.update(&store, &signal)?;
    store.compact_if_needed();
    drop(store); // bulk import is over

    let store = DBStore::open(&config.db_path, StoreOptions { bulk_import: false });
    let app = App::new(store, index, daemon.reconnect()?);

    let query = Query::new(app.clone(), &metrics);
    query.update_mempool()?; // poll once before starting RPC server

    let rpc = RPC::start(config.rpc_addr, query.clone(), &metrics);
    while let None = signal.wait(Duration::from_secs(5)) {
        query.update_mempool()?;
        if tip != app.daemon().getbestblockhash()? {
            tip = app.index().update(app.write_store(), &signal)?;
        }
        rpc.notify();
    }
    rpc.exit();
    Ok(())
}

fn main() {
    let config = Config::from_args();
    if let Err(e) = run_server(&config) {
        eprintln!("server failed: {}", e.display_chain());
    }
}
