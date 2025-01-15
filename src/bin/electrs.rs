extern crate error_chain;
#[macro_use]
extern crate log;

extern crate electrs;

use crossbeam_channel::{self as channel};
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
    new_index::{precache, zmq, ChainQuery, FetchFrom, Indexer, Mempool, Query, Store},
    rest,
    signal::Waiter,
};

#[cfg(feature = "otlp-tracing")]
use electrs::otlp_trace;

#[cfg(feature = "liquid")]
use electrs::elements::AssetRegistry;
use electrs::metrics::MetricOpts;

fn fetch_from(config: &Config, store: &Store) -> FetchFrom {
    let mut jsonrpc_import = config.jsonrpc_import;
    if !jsonrpc_import {
        // switch over to jsonrpc after the initial sync is done
        jsonrpc_import = store.done_initial_sync();
    }

    if jsonrpc_import {
        // slower, uses JSONRPC (good for incremental updates)
        FetchFrom::Bitcoind
    } else {
        // faster, uses blk*.dat files (good for initial indexing)
        FetchFrom::BlkFiles
    }
}

fn run_server(config: Arc<Config>) -> Result<()> {
    let (block_hash_notify, block_hash_receive) = channel::bounded(1);
    let signal = Waiter::start(block_hash_receive);
    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();

    if let Some(zmq_addr) = config.zmq_addr.as_ref() {
        zmq::start(&format!("tcp://{zmq_addr}"), block_hash_notify);
    }

    let daemon = Arc::new(Daemon::new(
        &config.daemon_dir,
        &config.blocks_dir,
        config.daemon_rpc_addr,
        config.daemon_parallelism,
        config.cookie_getter(),
        config.network_type,
        signal.clone(),
        &metrics,
    )?);
    let store = Arc::new(Store::open(&config.db_path.join("newindex"), &config));
    let mut indexer = Indexer::open(
        Arc::clone(&store),
        fetch_from(&config, &store),
        &config,
        &metrics,
    );
    let mut tip = indexer.update(&daemon)?;

    let chain = Arc::new(ChainQuery::new(
        Arc::clone(&store),
        Arc::clone(&daemon),
        &config,
        &metrics,
    ));

    if let Some(ref precache_file) = config.precache_scripts {
        let precache_scripthashes = precache::scripthashes_from_file(precache_file.to_string())
            .expect("cannot load scripts to precache");
        precache::precache(&chain, precache_scripthashes);
    }

    let mempool = Arc::new(RwLock::new(Mempool::new(
        Arc::clone(&chain),
        &metrics,
        Arc::clone(&config),
    )));

    while !Mempool::update(&mempool, &daemon, &tip)? {
        // Mempool syncing was aborted because the chain tip moved;
        // Index the new block(s) and try again.
        tip = indexer.update(&daemon)?;
    }

    #[cfg(feature = "liquid")]
    let asset_db = config.asset_db_path.as_ref().map(|db_dir| {
        let asset_db = Arc::new(RwLock::new(AssetRegistry::new(db_dir.clone())));
        AssetRegistry::spawn_sync(asset_db.clone());
        asset_db
    });

    let query = Arc::new(Query::new(
        Arc::clone(&chain),
        Arc::clone(&mempool),
        Arc::clone(&daemon),
        Arc::clone(&config),
        #[cfg(feature = "liquid")]
        asset_db,
    ));

    // TODO: configuration for which servers to start
    let rest_server = rest::start(Arc::clone(&config), Arc::clone(&query));
    let electrum_server = ElectrumRPC::start(Arc::clone(&config), Arc::clone(&query), &metrics);

    let main_loop_count = metrics.gauge(MetricOpts::new(
        "electrs_main_loop_count",
        "count of iterations of electrs main loop each 5 seconds or after interrupts",
    ));

    loop {
        main_loop_count.inc();

        if let Err(err) = signal.wait(Duration::from_secs(5), true) {
            info!("stopping server: {}", err);
            rest_server.stop();
            // the electrum server is stopped when dropped
            break;
        }

        // Index new blocks
        let current_tip = daemon.getbestblockhash()?;
        if current_tip != tip {
            tip = indexer.update(&daemon)?;
        };

        // Update mempool
        if !Mempool::update(&mempool, &daemon, &tip)? {
            warn!("skipped failed mempool update, trying again in 5 seconds");
        }

        // Update subscribed clients
        electrum_server.notify();
    }
    info!("server stopped");
    Ok(())
}

fn main_() {
    let config = Arc::new(Config::from_args());
    if let Err(e) = run_server(config) {
        error!("server failed: {}", e.display_chain());
        process::exit(1);
    }
}

#[cfg(not(feature = "otlp-tracing"))]
fn main() {
    main_();
}

#[cfg(feature = "otlp-tracing")]
#[tokio::main]
async fn main() {
    let _tracing_guard = otlp_trace::init_tracing("electrs");
    main_()
}
