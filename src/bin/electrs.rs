extern crate electrs;

extern crate error_chain;
#[macro_use]
extern crate log;

use error_chain::ChainedError;
use std::time::Duration;

use electrs::{app::App,
              bulk::Parser,
              config::Config,
              daemon::Daemon,
              errors::*,
              index::Index,
              metrics::Metrics,
              query::Query,
              rpc::RPC,
              signal::Waiter,
              store::{DBStore, StoreOptions}};

fn run_server(config: &Config) -> Result<()> {
    let signal = Waiter::new();
    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();

    let daemon = Daemon::new(config.network_type, &metrics)?;
    let store = DBStore::open(&config.db_path, StoreOptions { bulk_import: true });
    let parser = Parser::new(&daemon, &store, &metrics)?;
    store.bulk_load(parser, &signal)?;

    let daemon = daemon.reconnect()?;
    let store = DBStore::open(&config.db_path, StoreOptions { bulk_import: false });
    let index = Index::load(&store, &daemon, &metrics)?;
    let app = App::new(store, index, daemon);

    let query = Query::new(app.clone(), &metrics);
    let mut tip = *query.get_best_header()?.hash();
    let rpc = RPC::start(config.rpc_addr, query.clone(), &metrics);
    loop {
        query.update_mempool()?;
        if tip != app.daemon().getbestblockhash()? {
            tip = app.index().update(app.write_store(), &signal)?;
        }
        rpc.notify();
        if signal.wait(Duration::from_secs(5)).is_some() {
            break;
        }
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
