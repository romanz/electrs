#[macro_use]
extern crate log;

use anyhow::{Context, Result};
use bitcoin::{Address, OutPoint, Script, Txid};
use rayon::prelude::*;

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use electrs_index::*;

fn get_received(txs: &HashMap<Txid, Confirmed>, script: &Script) -> HashMap<OutPoint, u64> {
    txs.iter()
        .flat_map(|(txid, confirmed)| {
            confirmed
                .tx
                .output
                .iter()
                .enumerate()
                .filter_map(move |(i, txo)| {
                    if txo.script_pubkey == *script {
                        let outpoint = OutPoint {
                            txid: *txid,
                            vout: i as u32,
                        };
                        Some((outpoint, txo.value))
                    } else {
                        None
                    }
                })
        })
        .collect()
}

fn get_spent<F>(txs: &HashMap<Txid, Confirmed>, is_received: F) -> HashMap<OutPoint, Txid>
where
    F: Fn(&OutPoint) -> bool,
{
    txs.iter()
        .flat_map(|(txid, confirmed)| {
            confirmed
                .tx
                .input
                .iter()
                .filter(|txi| is_received(&txi.previous_output))
                .map(move |txi| (txi.previous_output, *txid))
        })
        .collect()
}

fn address_balance<'a>(
    index: &Index,
    daemon: &Daemon,
    address: &'a Address,
) -> Result<Vec<(usize, OutPoint, u64, &'a Address)>> {
    let script = &address.payload.script_pubkey();

    let confirmed: Vec<Confirmed> = index
        .lookup(&ScriptHash::new(script), daemon)?
        .readers
        .into_par_iter()
        .map(|r| r.read())
        .collect::<Result<_>>()?;

    let txs: HashMap<Txid, Confirmed> = confirmed
        .into_iter()
        .map(|confirmed| (confirmed.txid, confirmed))
        .collect();

    let received_map = get_received(&txs, script);
    let received_value = received_map.values().sum::<u64>();
    debug!(
        "received: {:.8} @ {} outputs (scanned {} txs, {} outputs)",
        received_value as f64 / 1e8,
        received_map.len(),
        txs.len(),
        txs.values()
            .map(|confirmed| confirmed.tx.output.len())
            .sum::<usize>(),
    );

    let spent_map = get_spent(&txs, |outpoint| received_map.contains_key(outpoint));
    let spent_value = spent_map
        .keys()
        .map(|outpoint| received_map.get(outpoint).expect("missing TXO"))
        .sum::<u64>();
    debug!(
        "spent:    {:.8} @ {} inputs (scanned {} txs, {} inputs)",
        spent_value as f64 / 1e8,
        spent_map.len(),
        txs.len(),
        txs.values()
            .map(|confirmed| confirmed.tx.input.len())
            .sum::<usize>()
    );

    let unspent_map: HashMap<OutPoint, u64> = received_map
        .into_iter()
        .filter(|(outpoint, _)| !spent_map.contains_key(outpoint))
        .collect();

    Ok(unspent_map
        .into_iter()
        .map(|(outpoint, value)| {
            let confirmed = txs.get(&outpoint.txid).unwrap();
            (confirmed.height, outpoint, value, address)
        })
        .collect())
}

fn query_index(index: &Index, daemon: &Daemon, addresses: &[Address]) -> Result<()> {
    if addresses.is_empty() {
        return Ok(());
    }
    let mut unspent = addresses
        .par_iter()
        .map(|address| address_balance(index, daemon, address))
        .collect::<Result<Vec<Vec<_>>>>()
        .context("failed to query address")?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    unspent.sort();
    let total: u64 = unspent
        .iter()
        .map(|(height, outpoint, value, address)| {
            info!(
                "{}:{:<5} {:20.8} @ {} {}",
                outpoint.txid,
                outpoint.vout,
                *value as f64 / 1e8,
                height,
                address,
            );
            value
        })
        .sum();
    info!("total: {:.8} BTC", total as f64 / 1e8);
    Ok(())
}

fn main() -> Result<()> {
    let config = Config::from_args();

    let daemon = Daemon::new(config.daemon_rpc_addr, &config.daemon_dir)
        .context("failed to connect to daemon")?;
    let metrics = Metrics::new(config.monitoring_addr)?;
    let store = DBStore::open(Path::new(&config.db_path))?;
    let index = Index::new(store, &metrics, config.low_memory).context("failed to open index")?;

    let addresses: Vec<Address> = config
        .args
        .iter()
        .map(|a| Address::from_str(a).expect("invalid address"))
        .collect();

    loop {
        let tip = index.update(&daemon).context("failed to update index")?;
        query_index(&index, &daemon, &addresses)?;
        while daemon.wait_for_new_block(Duration::from_secs(60))? == tip {}
    }
}
