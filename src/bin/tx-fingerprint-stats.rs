extern crate electrs;

#[cfg(not(feature = "liquid"))]
#[macro_use]
extern crate log;

#[cfg(not(feature = "liquid"))]
fn main() {
    use std::collections::HashSet;
    use std::sync::Arc;

    use bitcoin::blockdata::script::ScriptBuf;
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

    let signal = Waiter::start();
    let config = Config::from_args();
    let store = Arc::new(Store::open(&config.db_path.join("newindex"), &config));

    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();

    let daemon = Arc::new(
        Daemon::new(
            &config.daemon_dir,
            &config.blocks_dir,
            config.daemon_rpc_addr,
            config.cookie_getter(),
            config.network_type,
            signal,
            &metrics,
        )
        .unwrap(),
    );

    let chain = ChainQuery::new(Arc::clone(&store), Arc::clone(&daemon), &config, &metrics);

    let mut indexer = Indexer::open(Arc::clone(&store), FetchFrom::Bitcoind, &config, &metrics);
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

        let tx: Transaction = deserialize(&value).expect("failed to parse Transaction");
        let txid = tx.txid();

        iter.next();

        // only consider transactions of exactly two outputs
        if tx.output.len() != 2 {
            continue;
        }
        // skip coinbase txs
        if tx.is_coinbase() {
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

        let total_out: u64 = tx.output.iter().map(|out| out.value.to_sat()).sum();
        let small_out = tx
            .output
            .iter()
            .map(|out| out.value.to_sat())
            .min()
            .unwrap();
        let large_out = tx
            .output
            .iter()
            .map(|out| out.value.to_sat())
            .max()
            .unwrap();

        let total_in: u64 = prevouts.values().map(|out| out.value.to_sat()).sum();
        let smallest_in = prevouts
            .values()
            .map(|out| out.value.to_sat())
            .min()
            .unwrap();

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
            let mut seen_spks = HashSet::new();
            prevouts
                .values()
                .any(|out| !seen_spks.insert(&out.script_pubkey))
        };

        // test for sending back to one of the spent spks
        let has_reuse = {
            let prev_spks: HashSet<ScriptBuf> = prevouts
                .values()
                .map(|out| out.script_pubkey.clone())
                .collect();
            tx.output
                .iter()
                .any(|out| prev_spks.contains(&out.script_pubkey))
        };

        println!(
            "{},{},{},{},{},{}",
            txid, blockid.height, tx.lock_time, uih, is_multi_spend as u8, has_reuse as u8
        );

        total += 1;
        uih_totals[uih] += 1;
    }
    info!(
        "processed {} total txs, UIH counts: {:?}",
        total, uih_totals
    );
}

#[cfg(feature = "liquid")]
fn main() {}
