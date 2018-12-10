extern crate bitcoin;
extern crate error_chain;
#[macro_use]
extern crate log;

extern crate electrs;

use error_chain::ChainedError;
use std::process;
use std::str::FromStr;

use bitcoin::util::address::Address;

use electrs::{
    config::Config,
    daemon::Daemon,
    errors::*,
    metrics::Metrics,
    new_index::{FetchFrom, Indexer, Store},
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
    let store = Store::open(&config.db_path.join("newindex"));
    let mut indexer = Indexer::open(&store);
    let fetch = match config.jsonrpc_import {
        true => FetchFrom::BITCOIND, // slower, uses JSONRPC (good for incremental updates)
        false => FetchFrom::BLKFILES, // faster, uses blk*.dat files (good for initial indexing)
    };
    indexer.update(&daemon, fetch)?;
    let addr = Address::from_str("msRnv37GmMXU86EbPZTkGCCqYw1zUZX6v6").unwrap();
    for (txid, (txn, b)) in indexer.history(&addr.script_pubkey()) {
        info!("{} in {:?} --- {:?}", txid, b, txn);
    }

    debug!("utxo: {:?}", indexer.utxo(&addr.script_pubkey()));

    Ok(())
}

fn main() {
    let config = Config::from_args();
    if let Err(e) = run_server(config) {
        error!("server failed: {}", e.display_chain());
        process::exit(1);
    }
}
