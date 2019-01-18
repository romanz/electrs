extern crate bitcoin;
extern crate error_chain;
#[macro_use]
extern crate log;

extern crate electrs;

use error_chain::ChainedError;
use std::process;
use std::sync::Arc;
use std::time::Duration;

use electrs::{
    config::Config,
    daemon::Daemon,
    errors::*,
    metrics::Metrics,
    new_index::{FetchFrom, Indexer, Mempool, Query, Store},
    rest,
    signal::Waiter,
};

fn fetch_from(config: &Config, store: &Store) -> FetchFrom {
    let mut jsonrpc_import = config.jsonrpc_import;
    if !jsonrpc_import {
        jsonrpc_import = !store.is_empty();
    }
    match jsonrpc_import {
        true => FetchFrom::Bitcoind, // slower, uses JSONRPC (good for incremental updates)
        false => FetchFrom::BlkFiles, // faster, uses blk*.dat files (good for initial indexing)
    }
}

fn finish_verification(daemon: &Daemon, signal: &Waiter) -> Result<()> {
    loop {
        let progress = daemon.getblockchaininfo()?.verificationprogress;
        if progress > 0.9999 {
            return Ok(());
        }
        warn!(
            "waiting for verification to finish: {:.3}%",
            progress * 100.0
        );
        signal.wait(Duration::from_secs(5))?;
    }
}

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
    finish_verification(&daemon, &signal)?;
    let store = Arc::new(Store::open(&config.db_path.join("newindex")));
    let mut indexer = Indexer::open(Arc::clone(&store), fetch_from(&config, &store), &metrics);
    let mut tip = indexer.update(&daemon)?;

    let q = Arc::new(Query::new(Arc::clone(&store), &metrics));
    let mut mempool = Mempool::new(Arc::clone(&q));
    mempool.update(&daemon)?;

    let server = rest::run_server(&config, q);

    loop {
        if let Err(err) = signal.wait(Duration::from_secs(5)) {
            info!("stopping server: {}", err);
            server.stop();
            break;
        }
        let current_tip = daemon.getbestblockhash()?;
        if current_tip != tip {
            indexer.update(&daemon)?;
            tip = current_tip;
        };
        mempool.update(&daemon)?;
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
