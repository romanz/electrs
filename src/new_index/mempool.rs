use bitcoin::consensus::encode::serialize;
use bitcoin::util::hash::Sha256dHash;
use itertools::Itertools;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::iter::FromIterator;
use std::sync::Arc;

use crate::chain::{OutPoint, Transaction, TxOut};
use crate::daemon::Daemon;
use crate::errors::*;
use crate::metrics::{GaugeVec, HistogramOpts, HistogramVec, MetricOpts, Metrics};
use crate::new_index::{
    compute_script_hash, parse_hash, schema::FullHash, ChainQuery, FundingInfo, ScriptStats,
    SpendingInfo, SpendingInput, TxHistoryInfo, Utxo,
};
use crate::util::{has_prevout, Bytes};

pub struct Mempool {
    chain: Arc<ChainQuery>,
    txstore: HashMap<Sha256dHash, Transaction>,
    history: HashMap<FullHash, Vec<TxHistoryInfo>>, // ScriptHash -> {history_entries}
    edges: HashMap<OutPoint, (Sha256dHash, u32)>,   // OutPoint -> (spending_txid, spending_vin)
    // monitoring
    latency: HistogramVec, // mempool requests latency
    delta: HistogramVec,   // # of added/removed txs
    count: GaugeVec,       // current state of the mempool
}

impl Mempool {
    pub fn new(chain: Arc<ChainQuery>, metrics: &Metrics) -> Self {
        Mempool {
            chain,
            txstore: HashMap::new(),
            history: HashMap::new(),
            edges: HashMap::new(),
            latency: metrics.histogram_vec(
                HistogramOpts::new("mempool_latency", "Mempool requests latency (in seconds)"),
                &["part"],
            ),
            delta: metrics.histogram_vec(
                HistogramOpts::new("mempool_delta", "# of transactions added/removed"),
                &["type"],
            ),
            count: metrics.gauge_vec(
                MetricOpts::new("mempool_count", "# of elements currently at the mempool"),
                &["type"],
            ),
        }
    }

    pub fn lookup_txn(&self, txid: &Sha256dHash) -> Option<Transaction> {
        self.txstore.get(txid).map(|item| item.clone())
    }

    pub fn lookup_raw_txn(&self, txid: &Sha256dHash) -> Option<Bytes> {
        self.txstore.get(txid).map(serialize)
    }

    pub fn lookup_spend(&self, outpoint: &OutPoint) -> Option<SpendingInput> {
        self.edges.get(outpoint).map(|(txid, vin)| SpendingInput {
            txid: *txid,
            vin: *vin,
            confirmed: None,
        })
    }

    pub fn has_spend(&self, outpoint: &OutPoint) -> bool {
        self.edges.get(outpoint).is_some()
    }

    pub fn history(&self, scripthash: &[u8], limit: usize) -> Vec<Transaction> {
        let _timer = self.latency.with_label_values(&["history"]).start_timer();
        match self.history.get(scripthash) {
            None => return vec![],
            Some(entries) => entries
                .iter()
                .map(get_entry_txid)
                .unique()
                .take(limit)
                .map(|txid| self.txstore.get(&txid).expect("missing mempool tx"))
                .cloned()
                .collect(),
        }
    }

    pub fn utxo(&self, scripthash: &[u8]) -> Vec<Utxo> {
        let _timer = self.latency.with_label_values(&["utxo"]).start_timer();
        let entries = match self.history.get(scripthash) {
            None => return vec![],
            Some(entries) => entries,
        };

        entries
            .into_iter()
            .filter_map(|entry| match entry {
                TxHistoryInfo::Funding(info) => Some(Utxo {
                    txid: parse_hash(&info.txid),
                    vout: info.vout as u32,
                    value: info.value,
                    confirmed: None,
                }),
                TxHistoryInfo::Spending(..) => None,
            })
            .filter(|utxo| !self.has_spend(&OutPoint::from(utxo)))
            .collect()
    }

    // @XXX avoid code duplication with ChainQuery::stats()?
    pub fn stats(&self, scripthash: &[u8]) -> ScriptStats {
        let _timer = self.latency.with_label_values(&["stats"]).start_timer();
        let mut stats = ScriptStats::default();
        let mut seen_txids = HashSet::new();

        let entries = match self.history.get(scripthash) {
            None => return stats,
            Some(entries) => entries,
        };

        for entry in entries {
            if seen_txids.insert(get_entry_txid(entry)) {
                stats.tx_count += 1;
            }

            match entry {
                #[cfg(not(feature = "liquid"))]
                TxHistoryInfo::Funding(info) => {
                    stats.funded_txo_count += 1;
                    stats.funded_txo_sum += info.value;
                }
                #[cfg(feature = "liquid")]
                TxHistoryInfo::Funding(_) => {
                    stats.funded_txo_count += 1;
                }

                #[cfg(not(feature = "liquid"))]
                TxHistoryInfo::Spending(info) => {
                    stats.spent_txo_count += 1;
                    stats.spent_txo_sum += info.value;
                }
                #[cfg(feature = "liquid")]
                TxHistoryInfo::Spending(_) => {
                    stats.spent_txo_count += 1;
                }
            };
        }

        stats
    }

    pub fn update(&mut self, daemon: &Daemon) -> Result<()> {
        let _timer = self.latency.with_label_values(&["update"]).start_timer();
        let new_txids = daemon
            .getmempooltxids()
            .chain_err(|| "failed to update mempool from daemon")?;
        let old_txids = HashSet::from_iter(self.txstore.keys().cloned());
        let to_remove: HashSet<&Sha256dHash> = old_txids.difference(&new_txids).collect();

        // Download and add new transactions from bitcoind's mempool
        let txids: Vec<&Sha256dHash> = new_txids.difference(&old_txids).collect();
        let to_add = match daemon.gettransactions(&txids) {
            Ok(txs) => txs,
            Err(err) => {
                warn!("failed to get transactions {:?}: {}", txids, err); // e.g. new block or RBF
                return Ok(()); // keep the mempool until next update()
            }
        };
        // Add new transactions
        self.add(to_add);
        // Remove missing transactions
        self.remove(to_remove);

        self.count
            .with_label_values(&["txs"])
            .set(self.txstore.len() as f64);
        Ok(())
    }

    fn add(&mut self, txs: Vec<Transaction>) {
        self.delta
            .with_label_values(&["add"])
            .observe(txs.len() as f64);
        let _timer = self.latency.with_label_values(&["add"]).start_timer();

        let mut txids = vec![];
        // Phase 1: add to txstore
        for tx in txs {
            let txid = tx.txid();
            txids.push(txid);
            self.txstore.insert(txid, tx);
        }
        // Phase 2: index history and spend edges (can fail if some txos cannot be found)
        let txos = match self.lookup_txos(&self.get_prevouts(&txids)) {
            Ok(txos) => txos,
            Err(err) => {
                warn!("lookup txouts failed: {}", err);
                // TODO: should we remove txids from txstore?
                return;
            }
        };
        for txid in txids {
            let tx = self.txstore.get(&txid).expect("missing mempool tx");
            let txid_bytes = txid.into_bytes();
            // An iterator over (ScriptHash, TxHistoryInfo)
            let spending = tx.input.iter().enumerate().map(|(index, txi)| {
                let prev_txo = txos
                    .get(&txi.previous_output)
                    .expect(&format!("missing outpoint {:?}", txi.previous_output));
                (
                    compute_script_hash(&prev_txo.script_pubkey),
                    TxHistoryInfo::Spending(SpendingInfo {
                        txid: txid_bytes,
                        vin: index as u16,
                        prev_txid: txi.previous_output.txid.into_bytes(),
                        prev_vout: txi.previous_output.vout as u16,
                        value: prev_txo.value,
                    }),
                )
            });
            // An iterator over (ScriptHash, TxHistoryInfo)
            let funding = tx.output.iter().enumerate().map(|(index, txo)| {
                (
                    compute_script_hash(&txo.script_pubkey),
                    TxHistoryInfo::Funding(FundingInfo {
                        txid: txid_bytes,
                        vout: index as u16,
                        value: txo.value,
                    }),
                )
            });

            for (scripthash, entry) in funding.chain(spending) {
                self.history
                    .entry(scripthash)
                    .or_insert_with(|| Vec::new())
                    .push(entry);
            }

            for (i, txi) in tx.input.iter().enumerate() {
                self.edges.insert(txi.previous_output, (txid, i as u32));
            }
        }
    }

    // @TODO use parallel lookup for confirmed txos?
    pub fn lookup_txos(&self, outpoints: &BTreeSet<OutPoint>) -> Result<HashMap<OutPoint, TxOut>> {
        let _timer = self
            .latency
            .with_label_values(&["lookup_txos"])
            .start_timer();
        outpoints
            .into_iter()
            .map(|outpoint| {
                let result = match self.txstore.get(&outpoint.txid) {
                    Some(txn) => txn.output.get(outpoint.vout as usize).cloned(),
                    // TODO: do concurrently for non-mempool txns
                    None => self.chain.lookup_txo(&outpoint),
                };
                match result {
                    Some(txout) => Ok((*outpoint, txout)),
                    None => bail!("missing outpoint {:?}", outpoint),
                }
            })
            .collect()
    }

    fn get_prevouts(&self, txids: &[Sha256dHash]) -> BTreeSet<OutPoint> {
        txids
            .iter()
            .map(|txid| self.txstore.get(&txid).expect("missing mempool tx"))
            .flat_map(|tx| {
                tx.input
                    .iter()
                    .filter(|txin| has_prevout(txin))
                    .map(|txin| txin.previous_output)
            })
            .collect()
    }

    fn remove(&mut self, to_remove: HashSet<&Sha256dHash>) {
        self.delta
            .with_label_values(&["remove"])
            .observe(to_remove.len() as f64);
        let _timer = self.latency.with_label_values(&["remove"]).start_timer();

        for txid in &to_remove {
            self.txstore
                .remove(&txid)
                .expect(&format!("missing mempool tx {}", txid));
        }
        // TODO: make it more efficient (currently it takes O(|mempool|) time)
        self.history.retain(|_scripthash, entries| {
            entries.retain(|entry| !to_remove.contains(&get_entry_txid(entry)));
            !entries.is_empty()
        });

        self.edges
            .retain(|_outpoint, (txid, _vin)| !to_remove.contains(txid));
    }
}

fn get_entry_txid(entry: &TxHistoryInfo) -> Sha256dHash {
    match entry {
        TxHistoryInfo::Funding(info) => parse_hash(&info.txid),
        TxHistoryInfo::Spending(info) => parse_hash(&info.txid),
    }
}
