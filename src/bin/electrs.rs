extern crate electrs;

extern crate error_chain;
#[macro_use]
extern crate log;

use error_chain::ChainedError;
use std::process;
use std::sync::Arc;
use std::time::Duration;
use std::collections::HashSet;
use bitcoin_hashes::sha256d::Hash as Sha256dHash;
use bitcoin_hashes::Hash;

use electrs::{
    app::App,
    bulk,
    cache::{BlockTxIDsCache, TransactionCache},
    config::Config,
    daemon::{Daemon, tx_from_value},
    errors::*,
    index::{Index, compute_script_hash},
    metrics::Metrics,
    query::Query,
    rpc::RPC,
    signal::Waiter,
    store::{full_compaction, is_fully_compacted, DBStore},
};
use bitcoin::Transaction;

fn run_server(config: &Config) -> Result<()> {
    let signal = Waiter::start();
    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();
    let blocktxids_cache = Arc::new(BlockTxIDsCache::new(config.blocktxids_cache_size, &metrics));

    let daemon = Daemon::new(
        &config.daemon_dir,
        config.daemon_rpc_addr,
        config.cookie_getter(),
        config.network_type,
        signal.clone(),
        blocktxids_cache,
        &metrics,
    )?;
    // Perform initial indexing from local blk*.dat block files.
    let store = DBStore::open(&config.db_path, /*low_memory=*/ config.jsonrpc_import);
    let index = Index::load(&store, &daemon, &metrics, config.index_batch_size)?;
    let store = if is_fully_compacted(&store) {
        store // initial import and full compaction are over
    } else if config.jsonrpc_import {
        index.update(&store, &signal)?; // slower: uses JSONRPC for fetching blocks
        full_compaction(store)
    } else {
        // faster, but uses more memory
        let store =
            bulk::index_blk_files(&daemon, config.bulk_index_threads, &metrics, &signal, store)?;
        let store = full_compaction(store);
        index.reload(&store); // make sure the block header index is up-to-date
        store
    }
    .enable_compaction(); // enable auto compactions before starting incremental index updates.

    let app = App::new(store, index, daemon, &config)?;
    let tx_cache = TransactionCache::new(config.tx_cache_size, &metrics);
    let query = Query::new(app.clone(), &metrics, tx_cache, config.txid_limit);

    let mut server = None; // Electrum RPC server
    loop {
        let new_block_headers = app.update(&signal)?;
        let new_mempool_txs = query.update_mempool()?;
        let script_hashes = get_script_hashes_in_blocks(app.daemon(), &query,new_block_headers)?
            .extend(get_script_hashes_in_mempool_txs(app.daemon(), &query, new_mempool_txs)?);
        server
            .get_or_insert_with(|| RPC::start(config.electrum_rpc_addr, query.clone(), &metrics))
            .notify(); // update subscribed clients
        if let Err(err) = signal.wait(Duration::from_secs(5)) {
            info!("stopping server: {}", err);
            break;
        }
    }
    Ok(())
}


fn get_script_hashes_in_blocks(daemon: &Daemon, query: &Arc<Query>, block_hashes: Vec<Sha256dHash>) -> Result<HashSet<Sha256dHash>> {
    let mut script_hashes = HashSet::<Sha256dHash>::new();
    let blocks = daemon.getblocks(block_hashes.as_ref())?;
    for block in blocks {
        for tx in block.txdata {
            if tx.is_coin_base() {
                continue;
            }

            // for each script_hash in tx.inputs
            for input in tx.input.iter() {
                // find output, get the relevant script hash in it
                let previous_output_tx = tx_from_value(
                    query.get_transaction(&input.previous_output.txid, false)?)?;
                let previous_output = previous_output_tx.output.get(input.previous_output.vout as usize)
                    .chain_err(|| format!("failed finding previous output {}:{}", input.previous_output.txid, input.previous_output.vout))?;
                let script_hash =
                    Sha256dHash::from_slice(&compute_script_hash(&previous_output.script_pubkey[..]))
                        .expect("failed computing script hash for output.script_pubkey");
                // if script_hash in self.script_hashes, add to the relevant script hashes list
                script_hashes.insert(script_hash);
            }

            for (i, output) in tx.output.iter().enumerate() {
                let script_hash =
                    Sha256dHash::from_slice(&compute_script_hash(&output.script_pubkey[..]))
                        .expect(&format!("failed computing script hash for output {}:{}", tx.txid(), i));
                // if script_hash in self.script_hashes:
                script_hashes.insert(script_hash);
            }
        }
    }

    Ok(script_hashes)
}

fn get_script_hashes_in_mempool_txs(daemon: &Daemon, query: &Arc<Query>, txs: Vec<Transaction>) -> Result<HashSet<Sha256dHash>> {
    let mut script_hashes: HashSet<Sha256dHash> = HashSet::<Sha256dHash>::new();
    for tx in txs {
        for input in tx.input.iter() {
            // find output, get the relevant script hash in it
            let previous_output = daemon.get_confirmed_utxo(&input.previous_output.txid, input.previous_output.vout)
                .or_else(|_e| {
                    // possibly failed because previous output's transaction itself is unconfirmed, so search in mempool.
                    debug!("failed to find a confirmed previous output {}:{}", &input.previous_output.txid, &input.previous_output.vout);
                    let previous_output_tx: Transaction = query.tracker().read().unwrap()
                        .get_txn(&input.previous_output.txid)
                        .chain_err(|| format!("failed to find unconfirmed previous output tx {}:{}",
                            &input.previous_output.txid, &input.previous_output.vout))?;

                    previous_output_tx.output.get(input.previous_output.vout as usize)
                        .map(|output| output.clone())
                        .chain_err(|| format!("failed to find previous output index in tx {}:{}",
                            &input.previous_output.txid, &input.previous_output.vout))
                })
                .expect(&format!("failed to get previous output {}:{}", &input.previous_output.txid, &input.previous_output.vout));

            let script_hash =
                Sha256dHash::from_slice(&compute_script_hash(&previous_output.script_pubkey[..]))
                    .expect(&format!("failed computing script hash of previous output {}:{}",
                        &input.previous_output.txid, &input.previous_output.vout));

            script_hashes.insert(script_hash);
        }

        for (i, output) in tx.output.iter().enumerate() {
            let script_hash =
                Sha256dHash::from_slice(&compute_script_hash(&output.script_pubkey[..]))
                    .expect(&format!("failed computing script hash for output {}:{}", tx.txid(), i));

            script_hashes.insert(script_hash);
        }
    }

    Ok(script_hashes)
}

fn main() {
    let config = Config::from_args();
    if let Err(e) = run_server(&config) {
        error!("server failed: {}", e.display_chain());
        process::exit(1);
    }
}
