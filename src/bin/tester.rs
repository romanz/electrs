extern crate bitcoin;
extern crate error_chain;
#[macro_use]
extern crate log;

extern crate electrs;

use bitcoin::util::hash::Sha256dHash;
use error_chain::ChainedError;
use std::process;
use std::sync::Arc;
use std::time::Duration;

use electrs::{
    config::Config,
    daemon::Daemon,
    errors::*,
    metrics::Metrics,
    new_index::{FetchFrom, Indexer, Query, Store},
    rest,
    signal::Waiter,
};

fn run_server(config: Config) -> Result<()> {
    let signal = Waiter::new();
    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();

    let daemon = Daemon::new(
        &config.daemon_dir,
        config.daemon_rpc_addr,
        config.cookie_getter(),
        config.network_type,
        signal.clone(),
        &metrics,
    )?;
    let store = Arc::new(Store::open(&config.db_path.join("newindex")));
    let mut indexer = Indexer::open(Arc::clone(&store));
    let fetch = match config.jsonrpc_import {
        true => FetchFrom::BITCOIND, // slower, uses JSONRPC (good for incremental updates)
        false => FetchFrom::BLKFILES, // faster, uses blk*.dat files (good for initial indexing)
    };
    indexer.update(&daemon, fetch)?;
    indexer.start_flushing();
    let q = Query::new(Arc::clone(&store));
    let server = rest::run_server(&config, Arc::new(q));

    let mut tip = Sha256dHash::default();
    loop {
        let current_tip = daemon.getbestblockhash()?;
        if current_tip != tip {
            indexer.update(&daemon, fetch)?;
            tip = current_tip;
        } else {
            if let Err(err) = signal.wait(Duration::from_secs(5)) {
                info!("stopping server: {}", err);
                break;
            }
        }
    }
    info!("server stopped");
    Ok(())
}

fn main() {
    let config = Config::from_args();
    if let Err(e) = run_server(config) {
        error!("server failed: {}", e.display_chain());
        process::exit(1);
    }
}
