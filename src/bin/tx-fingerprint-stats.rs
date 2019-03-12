extern crate electrs;
#[macro_use]
extern crate log;

use std::sync::Arc;
use std::collections::HashSet;

use bitcoin::consensus::encode::deserialize;
use electrs::{
    chain::Transaction,
    config::Config,
    daemon::Daemon,
    metrics::Metrics,
    new_index::{ChainQuery, FetchFrom, Indexer, Store},
    signal::Waiter,
    util::has_prevout,
};

#[cfg(not(feature = "liquid"))]
fn main() {
    let signal = Waiter::new();
    let config = Config::from_args();
    let store = Arc::new(Store::open(&config.db_path.join("newindex")));

    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();

    let chain = ChainQuery::new(Arc::clone(&store), &metrics);

    let daemon = Arc::new(
        Daemon::new(
            &config.daemon_dir,
            config.daemon_rpc_addr,
            config.cookie_getter(),
            config.network_type,
            signal.clone(),
            &metrics,
        )
        .unwrap(),
    );

    let mut indexer = Indexer::open(Arc::clone(&store), FetchFrom::Bitcoind, &metrics);
    indexer.update(&daemon).unwrap();

    let mut iter = store.txstore_db().raw_iterator();
    iter.seek(b"T");

    let mut total = 0;
    let mut uih_totals = vec![0, 0, 0];

    while iter.valid() {
        let key = iter.key().unwrap();
        let value = iter.value().unwrap();

        if !key.starts_with(b"T") {
            break;
        }

        iter.next();

        let tx: Transaction = deserialize(&value).expect("failed to parse Transaction");
        let txid = tx.txid();

        // only consider transactions of exactly two outputs
        if tx.output.len() != 2 {
            continue;
        }
        // skip coinbase txs
        if tx.is_coin_base() {
            continue;
        }

        // skip orphaned transactions
        let blockid = match chain.tx_confirming_block(&txid) {
            Some(blockid) => blockid,
            None => continue,
        };

        //info!("{:?},{:?}", txid, blockid);

        let prevouts = chain.lookup_txos(
            &tx.input
                .iter()
                .filter(|txin| has_prevout(txin))
                .map(|txin| txin.previous_output)
                .collect(),
        );

        let total_out: u64 = tx.output.iter().map(|out| out.value).sum();
        let small_out = tx.output.iter().map(|out| out.value).min().unwrap();
        let large_out = tx.output.iter().map(|out| out.value).max().unwrap();

        let total_in: u64 = prevouts.values().map(|out| out.value).sum();
        let smallest_in = prevouts.values().map(|out| out.value).min().unwrap();

        let fee = total_in - total_out;

        // test for UIH
        let uih = if total_in - smallest_in > large_out + fee {
            2
        } else if total_in - smallest_in > small_out + fee {
            1
        } else {
            0
        };

        // test for spending multiple coins owned by the same spk
        let is_multi_spend = {
            let mut seen = HashSet::new();
            prevouts.values().any(|out| !seen.insert(&out.script_pubkey))
        };

        println!(
            "{},{},{},{},{}",
            txid, blockid.height, tx.lock_time, uih, is_multi_spend as u8
        );

        total = total + 1;
        uih_totals[uih] = uih_totals[uih] + 1;
    }
    info!(
        "processed {} total txs, UIH counts: {:?}",
        total, uih_totals
    );
}

#[cfg(feature = "liquid")]
fn main() {}
