use arraydeque::{ArrayDeque, Wrapping};
use bitcoin::consensus::encode::serialize;
use bitcoin_hashes::sha256d::Hash as Sha256dHash;
use itertools::Itertools;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::iter::FromIterator;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::chain::{OutPoint, Transaction, TxOut};
use crate::daemon::Daemon;
use crate::errors::*;
use crate::metrics::{GaugeVec, HistogramOpts, HistogramVec, MetricOpts, Metrics};
use crate::new_index::{
    compute_script_hash, parse_hash, schema::FullHash, ChainQuery, FundingInfo, ScriptStats,
    SpendingInfo, SpendingInput, TxHistoryInfo, Utxo,
};
use crate::util::fees::{make_fee_histogram, TxFeeInfo};
use crate::util::{full_hash, has_prevout, is_spendable, Bytes};

const RECENT_TXS_SIZE: usize = 10;
const BACKLOG_STATS_TTL: u64 = 10;

pub struct Mempool {
    chain: Arc<ChainQuery>,
    txstore: HashMap<Sha256dHash, Transaction>,
    feeinfo: HashMap<Sha256dHash, TxFeeInfo>,
    history: HashMap<FullHash, Vec<TxHistoryInfo>>, // ScriptHash -> {history_entries}
    edges: HashMap<OutPoint, (Sha256dHash, u32)>,   // OutPoint -> (spending_txid, spending_vin)
    recent: ArrayDeque<[TxOverview; RECENT_TXS_SIZE], Wrapping>, // The N most recent txs to enter the mempool
    backlog_stats: (BacklogStats, Instant),

    // monitoring
    latency: HistogramVec, // mempool requests latency
    delta: HistogramVec,   // # of added/removed txs
    count: GaugeVec,       // current state of the mempool
}

// A simplified transaction view used for the list of most recent transactions
#[derive(Serialize)]
pub struct TxOverview {
    txid: Sha256dHash,
    fee: u64,
    vsize: u32,
    #[cfg(not(feature = "liquid"))]
    value: u64,
}

impl Mempool {
    pub fn new(chain: Arc<ChainQuery>, metrics: &Metrics) -> Self {
        Mempool {
            chain,
            txstore: HashMap::new(),
            feeinfo: HashMap::new(),
            history: HashMap::new(),
            edges: HashMap::new(),
            recent: ArrayDeque::new(),
            backlog_stats: (
                BacklogStats::default(),
                Instant::now() - Duration::from_secs(BACKLOG_STATS_TTL),
            ),
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
        self.edges.contains_key(outpoint)
    }

    // TODO: return as Vec<(Transaction,Option<BlockId>)>?
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

    pub fn history_txids(&self, scripthash: &[u8]) -> Vec<Sha256dHash> {
        let _timer = self
            .latency
            .with_label_values(&["history_txids"])
            .start_timer();
        match self.history.get(scripthash) {
            None => return vec![],
            Some(entries) => entries.iter().map(get_entry_txid).unique().collect(),
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

    // Get all txids in the mempool
    pub fn txids(&self) -> Vec<&Sha256dHash> {
        let _timer = self.latency.with_label_values(&["txids"]).start_timer();
        self.txstore.keys().collect()
    }

    // Get an overview of the most recent transactions
    pub fn recent_txs_overview(&self) -> Vec<&TxOverview> {
        // We don't bother ever deleting elements from the recent list.
        // It may contain outdated txs that are no longer in the mempool,
        // until they get pushed out by newer transactions.
        self.recent.iter().collect()
    }

    pub fn backlog_stats(&self) -> &BacklogStats {
        &self.backlog_stats.0
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

        // Update cached backlog stats (if expired)
        if self.backlog_stats.1.elapsed() > Duration::from_secs(BACKLOG_STATS_TTL) {
            let _timer = self
                .latency
                .with_label_values(&["update_backlog_stats"])
                .start_timer();
            self.backlog_stats = (BacklogStats::new(&self.feeinfo), Instant::now());
        }

        Ok(())
    }

    pub fn add_by_txid(&mut self, daemon: &Daemon, txid: &Sha256dHash) {
        if let Ok(tx) = daemon.getmempooltx(&txid) {
            self.add(vec![tx])
        }
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
            let txid_bytes = full_hash(&txid[..]);

            let prevouts: HashMap<u32, &TxOut> = tx
                .input
                .iter()
                .enumerate()
                .filter(|(_, txi)| has_prevout(txi))
                .map(|(index, txi)| {
                    (
                        index as u32,
                        txos.get(&txi.previous_output)
                            .expect(&format!("missing outpoint {:?}", txi.previous_output)),
                    )
                })
                .collect();

            // Get feeinfo for caching and recent tx overview
            let feeinfo = TxFeeInfo::new(&tx, &prevouts);

            // recent is an ArrayDeque that automatically evicts the oldest elements
            self.recent.push_front(TxOverview {
                txid: txid,
                fee: feeinfo.fee,
                vsize: feeinfo.vsize,
                #[cfg(not(feature = "liquid"))]
                value: prevouts.values().map(|prevout| prevout.value).sum(),
            });

            self.feeinfo.insert(txid, feeinfo);

            // An iterator over (ScriptHash, TxHistoryInfo)
            let spending = prevouts.into_iter().map(|(input_index, prevout)| {
                let txi = tx.input.get(input_index as usize).unwrap();
                (
                    compute_script_hash(&prevout.script_pubkey),
                    TxHistoryInfo::Spending(SpendingInfo {
                        txid: txid_bytes,
                        vin: input_index as u16,
                        prev_txid: full_hash(&txi.previous_output.txid[..]),
                        prev_vout: txi.previous_output.vout as u16,
                        value: prevout.value,
                    }),
                )
            });

            // An iterator over (ScriptHash, TxHistoryInfo)
            let funding = tx
                .output
                .iter()
                .enumerate()
                .filter(|(_, txo)| is_spendable(txo))
                .map(|(index, txo)| {
                    (
                        compute_script_hash(&txo.script_pubkey),
                        TxHistoryInfo::Funding(FundingInfo {
                            txid: txid_bytes,
                            vout: index as u16,
                            value: txo.value,
                        }),
                    )
                });

            // Index funding/spending history entries and spend edges
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

    pub fn lookup_txos(&self, outpoints: &BTreeSet<OutPoint>) -> Result<HashMap<OutPoint, TxOut>> {
        let _timer = self
            .latency
            .with_label_values(&["lookup_txos"])
            .start_timer();

        let confirmed_txos = self.chain.lookup_avail_txos(outpoints);

        let mempool_txos = outpoints
            .iter()
            .filter(|outpoint| !confirmed_txos.contains_key(outpoint))
            .map(|outpoint| {
                self.txstore
                    .get(&outpoint.txid)
                    .and_then(|tx| tx.output.get(outpoint.vout as usize).cloned())
                    .map(|txout| (*outpoint, txout))
                    .chain_err(|| format!("missing outpoint {:?}", outpoint))
            })
            .collect::<Result<HashMap<OutPoint, TxOut>>>()?;

        let mut txos = confirmed_txos;
        txos.extend(mempool_txos);
        Ok(txos)
    }

    fn get_prevouts(&self, txids: &[Sha256dHash]) -> BTreeSet<OutPoint> {
        txids
            .iter()
            .map(|txid| self.txstore.get(txid).expect("missing mempool tx"))
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
                .remove(*txid)
                .expect(&format!("missing mempool tx {}", txid));
            self.feeinfo
                .remove(*txid)
                .expect(&format!("missing mempool tx feeinfo {}", txid));
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

#[derive(Serialize)]
pub struct BacklogStats {
    pub count: u32,
    pub vsize: u32,     // in virtual bytes (= weight/4)
    pub total_fee: u64, // in satoshis
    pub fee_histogram: Vec<(f32, u32)>,
}

impl BacklogStats {
    fn default() -> Self {
        BacklogStats {
            count: 0,
            vsize: 0,
            total_fee: 0,
            fee_histogram: vec![(0.0, 0)],
        }
    }

    fn new(feeinfo: &HashMap<Sha256dHash, TxFeeInfo>) -> Self {
        let (count, vsize, total_fee) = feeinfo
            .values()
            .fold((0, 0, 0), |(count, vsize, fee), feeinfo| {
                (count + 1, vsize + feeinfo.vsize, fee + feeinfo.fee)
            });

        BacklogStats {
            count,
            vsize,
            total_fee,
            fee_histogram: make_fee_histogram(feeinfo.values().collect()),
        }
    }
}

fn get_entry_txid(entry: &TxHistoryInfo) -> Sha256dHash {
    match entry {
        TxHistoryInfo::Funding(info) => parse_hash(&info.txid),
        TxHistoryInfo::Spending(info) => parse_hash(&info.txid),
    }
}
