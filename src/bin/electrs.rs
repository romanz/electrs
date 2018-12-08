extern crate electrs;

extern crate error_chain;
#[macro_use]
extern crate log;

use error_chain::ChainedError;
use std::process;
use std::time::Duration;

use electrs::{
    app::App,
    bulk,
    config::Config,
    daemon::Daemon,
    errors::*,
    index::Index,
    metrics::Metrics,
    signal::Waiter,
    store::{full_compaction, is_fully_compacted, verify_index_compatibility, DBStore},
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
    // Perform initial indexing from local blk*.dat block files.
    let store = DBStore::open(&config.db_path, /*low_memory=*/ config.jsonrpc_import);
    let index = Index::load(&store, &daemon, &metrics, &config)?;

    verify_index_compatibility(&store, &config);

    let store = if is_fully_compacted(&store) {
        store // initial import and full compaction are over
    } else {
        if config.jsonrpc_import {
            index.update(&store, &signal)?; // slower: uses JSONRPC for fetching blocks
            full_compaction(store)
        } else {
            // faster, but uses more memory
            let store = bulk::index_blk_files(&daemon, &config, &metrics, store)?;
            let store = full_compaction(store);
            index.reload(&store); // make sure the block header index is up-to-date
            store
        }
    }
    .enable_compaction(); // enable auto compactions before starting incremental index updates.

    let app = App::new(store, index, daemon)?;

    loop {
        app.update(&signal)?;
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
