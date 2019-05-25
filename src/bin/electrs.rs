extern crate bitcoin;
extern crate error_chain;
#[macro_use]
extern crate log;

extern crate electrs;

use error_chain::ChainedError;
use std::process;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use electrs::{
    config::Config,
    daemon::Daemon,
    electrum::RPC as ElectrumRPC,
    errors::*,
    metrics::Metrics,
    new_index::{precache, ChainQuery, FetchFrom, Indexer, Mempool, Query, Store},
    rest,
    signal::Waiter,
};

#[cfg(feature = "liquid")]
use electrs::elements::AssetRegistry;

fn fetch_from(config: &Config, store: &Store) -> FetchFrom {
    let mut jsonrpc_import = config.jsonrpc_import;
    if !jsonrpc_import {
        // switch over to jsonrpc after the initial sync is done
        jsonrpc_import = store.done_initial_sync();
    }
    match jsonrpc_import {
        true => FetchFrom::Bitcoind, // slower, uses JSONRPC (good for incremental updates)
        false => FetchFrom::BlkFiles, // faster, uses blk*.dat files (good for initial indexing)
    }
}

fn run_server(config: Arc<Config>) -> Result<()> {
    let signal = Waiter::new();
    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();

    let daemon = Arc::new(Daemon::new(
        &config.daemon_dir,
        config.daemon_rpc_addr,
        config.cookie_getter(),
        config.network_type,
        signal.clone(),
        &metrics,
    )?);
    let store = Arc::new(Store::open(&config.db_path.join("newindex")));
    let mut indexer = Indexer::open(Arc::clone(&store), fetch_from(&config, &store), &metrics);
    let mut tip = indexer.update(&daemon)?;

    let chain = Arc::new(ChainQuery::new(Arc::clone(&store), &metrics));

    if let Some(ref precache_file) = config.precache_scripts {
        let precache_scripthashes = precache::scripthashes_from_file(precache_file.to_string())
            .expect("cannot load scripts to precache");
        precache::precache(&chain, precache_scripthashes);
    }

    let mempool = Arc::new(RwLock::new(Mempool::new(Arc::clone(&chain), &metrics)));
    mempool.write().unwrap().update(&daemon)?;

    #[cfg(feature = "liquid")]
    let asset_db = config
        .asset_db_path
        .as_ref()
        .map(|dir| AssetRegistry::new(dir.clone()));

    let query = Arc::new(Query::new(
        Arc::clone(&chain),
        Arc::clone(&mempool),
        Arc::clone(&daemon),
        #[cfg(feature = "liquid")]
        asset_db,
    ));

    // TODO: configuration for which servers to start
    let rest_server = rest::run_server(Arc::clone(&config), Arc::clone(&query));
    let electrum_server =
        ElectrumRPC::start(config.electrum_rpc_addr, Arc::clone(&query), &metrics);

    loop {
        if let Err(err) = signal.wait(Duration::from_secs(5)) {
            info!("stopping server: {}", err);
            rest_server.stop();
            break;
        }

        // Index new blocks
        let current_tip = daemon.getbestblockhash()?;
        if current_tip != tip {
            indexer.update(&daemon)?;
            tip = current_tip;
        };

        // Update mempool
        mempool.write().unwrap().update(&daemon)?;

        // Update subscribed clients
        electrum_server.notify();
    }
    info!("server stopped");
    Ok(())
}

fn main() {
    let config = Arc::new(Config::from_args());
    if let Err(e) = run_server(config) {
        error!("server failed: {}", e.display_chain());
        process::exit(1);
    }
}
