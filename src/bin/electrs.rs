extern crate electrs;

#[macro_use]
extern crate error_chain;
#[macro_use]
extern crate log;

use error_chain::ChainedError;
use std::time::Duration;

use electrs::{
    app::App, bulk::Parser, config::Config, daemon::Daemon, errors::*, index::Index,
    metrics::Metrics, query::Query, rpc::RPC, signal::Waiter,
    store::{DBStore, ReadStore, StoreOptions, WriteStore},
};

fn bulk_load(store: DBStore, daemon: &Daemon, signal: &Waiter, metrics: &Metrics) -> Result<()> {
    let key = b"F"; // full compaction marker
    if store.get(key).is_some() {
        return Ok(());
    }
    let parser = Parser::new(daemon, &store, &metrics)?;
    for rows in parser.start().iter() {
        if let Some(sig) = signal.poll() {
            bail!("indexing interrupted by SIG{:?}", sig);
        }
        store.write(rows?);
    }
    store.flush();
    store.compact();
    store.put(key, b"");
    Ok(())
}

fn run_server(config: &Config) -> Result<()> {
    let signal = Waiter::new();
    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();

    let daemon = Daemon::new(
        &config.daemon_dir,
        &config.cookie,
        config.network_type,
        &metrics,
    )?;
    bulk_load(
        DBStore::open(&config.db_path, StoreOptions { bulk_import: true }),
        &daemon,
        &signal,
        &metrics,
    )?;

    let daemon = daemon.reconnect()?;
    let store = DBStore::open(&config.db_path, StoreOptions { bulk_import: false });
    let index = Index::load(&store, &daemon, &metrics)?;
    let app = App::new(store, index, daemon);
    let mut tip = app.index().update(app.write_store(), &signal)?;

    let query = Query::new(app.clone(), &metrics);
    query.update_mempool()?;

    let rpc = RPC::start(config.rpc_addr, query.clone(), &metrics);
    while let None = signal.wait(Duration::from_secs(5)) {
        if tip != app.daemon().getbestblockhash()? {
            tip = app.index().update(app.write_store(), &signal)?;
        }
        query.update_mempool()?;
        rpc.notify(); // update subscribed clients
    }
    rpc.exit();
    Ok(())
}

fn main() {
    let config = Config::from_args();
    if let Err(e) = run_server(&config) {
        error!("server failed: {}", e.display_chain());
    }
}
