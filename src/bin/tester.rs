extern crate bitcoin;
extern crate error_chain;
#[macro_use]
extern crate log;

extern crate electrs;

use error_chain::ChainedError;
use std::process;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bitcoin::util::address::Address;

use electrs::{
    config::Config,
    daemon::Daemon,
    errors::*,
    metrics::Metrics,
    new_index::{compute_script_hash, FetchFrom, Indexer, Query, Store},
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
    let script = Address::from_str("msRnv37GmMXU86EbPZTkGCCqYw1zUZX6v6")
        .unwrap()
        .script_pubkey();
    let scripthash = compute_script_hash(&script.as_bytes());
    let q = Query::new(Arc::clone(&store));

    for (tx, b) in q.history(&scripthash, None, 10) {
        info!("tx in {:?} --- {:?}", b, tx);
    }

    debug!("utxo: {:?}", q.utxo(&scripthash));

    let q = Arc::new(q);
    let server = rest::run_server(&config, q);

    loop {
        if let Err(err) = signal.wait(Duration::from_secs(5)) {
            info!("stopping server: {}", err);
            break;
        }
    }

    Ok(())
}

fn main() {
    let config = Config::from_args();
    if let Err(e) = run_server(config) {
        error!("server failed: {}", e.display_chain());
        process::exit(1);
    }
}
