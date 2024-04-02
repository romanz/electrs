use arraydeque::{ArrayDeque, Wrapping};
use itertools::Itertools;

#[cfg(not(feature = "liquid"))]
use bitcoin::consensus::encode::serialize;
#[cfg(feature = "liquid")]
use elements::{encode::serialize, AssetId};

use std::collections::{BTreeSet, HashMap, HashSet};
use std::iter::FromIterator;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::chain::{deserialize, Network, OutPoint, Transaction, TxOut, Txid};
use crate::config::Config;
use crate::daemon::Daemon;
use crate::errors::*;
use crate::metrics::{GaugeVec, HistogramOpts, HistogramVec, MetricOpts, Metrics};
use crate::new_index::{
    compute_script_hash, schema::FullHash, ChainQuery, FundingInfo, GetAmountVal, ScriptStats,
    SpendingInfo, SpendingInput, TxHistoryInfo, Utxo,
};
use crate::util::fees::{make_fee_histogram, TxFeeInfo};
use crate::util::{extract_tx_prevouts, full_hash, has_prevout, is_spendable, Bytes};

#[cfg(feature = "liquid")]
use crate::elements::asset;

const RECENT_TXS_SIZE: usize = 10;
const BACKLOG_STATS_TTL: u64 = 10;

pub struct Mempool {
    chain: Arc<ChainQuery>,
    config: Arc<Config>,
    txstore: HashMap<Txid, Transaction>,
    feeinfo: HashMap<Txid, TxFeeInfo>,
    history: HashMap<FullHash, Vec<TxHistoryInfo>>, // ScriptHash -> {history_entries}
    edges: HashMap<OutPoint, (Txid, u32)>,          // OutPoint -> (spending_txid, spending_vin)
    recent: ArrayDeque<TxOverview, RECENT_TXS_SIZE, Wrapping>, // The N most recent txs to enter the mempool
    backlog_stats: (BacklogStats, Instant),

    // monitoring
    latency: HistogramVec, // mempool requests latency
    delta: HistogramVec,   // # of added/removed txs
    count: GaugeVec,       // current state of the mempool

    // elements only
    #[cfg(feature = "liquid")]
    pub asset_history: HashMap<AssetId, Vec<TxHistoryInfo>>,
    #[cfg(feature = "liquid")]
    pub asset_issuance: HashMap<AssetId, asset::AssetRow>,
}

// A simplified transaction view used for the list of most recent transactions
#[derive(Serialize)]
pub struct TxOverview {
    txid: Txid,
    fee: u64,
    vsize: u64,
    #[cfg(not(feature = "liquid"))]
    value: u64,
}

impl Mempool {
    pub fn new(chain: Arc<ChainQuery>, metrics: &Metrics, config: Arc<Config>) -> Self {
        Mempool {
            chain,
            config,
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

            #[cfg(feature = "liquid")]
            asset_history: HashMap::new(),
            #[cfg(feature = "liquid")]
            asset_issuance: HashMap::new(),
        }
    }

    pub fn network(&self) -> Network {
        self.config.network_type
    }

    pub fn lookup_txn(&self, txid: &Txid) -> Option<Transaction> {
        self.txstore.get(txid).cloned()
    }

    pub fn lookup_raw_txn(&self, txid: &Txid) -> Option<Bytes> {
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

    pub fn get_tx_fee(&self, txid: &Txid) -> Option<u64> {
        Some(self.feeinfo.get(txid)?.fee)
    }

    pub fn has_unconfirmed_parents(&self, txid: &Txid) -> bool {
        let tx = match self.txstore.get(txid) {
            Some(tx) => tx,
            None => return false,
        };
        tx.input
            .iter()
            .any(|txin| self.txstore.contains_key(&txin.previous_output.txid))
    }

    pub fn history(&self, scripthash: &[u8], limit: usize) -> Vec<Transaction> {
        let _timer = self.latency.with_label_values(&["history"]).start_timer();
        self.history
            .get(scripthash)
            .map_or_else(|| vec![], |entries| self._history(entries, limit))
    }

    fn _history(&self, entries: &[TxHistoryInfo], limit: usize) -> Vec<Transaction> {
        entries
            .iter()
            .map(|e| e.get_txid())
            .unique()
            .take(limit)
            .map(|txid| self.txstore.get(&txid).expect("missing mempool tx"))
            .cloned()
            .collect()
    }

    pub fn history_txids(&self, scripthash: &[u8], limit: usize) -> Vec<Txid> {
        let _timer = self
            .latency
            .with_label_values(&["history_txids"])
            .start_timer();
        match self.history.get(scripthash) {
            None => vec![],
            Some(entries) => entries
                .iter()
                .map(|e| e.get_txid())
                .unique()
                .take(limit)
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
            .iter()
            .filter_map(|entry| match entry {
                TxHistoryInfo::Funding(info) => {
                    // Liquid requires some additional information from the txo that's not available in the TxHistoryInfo index.
                    #[cfg(feature = "liquid")]
                    let txo = self
                        .lookup_txo(&entry.get_funded_outpoint())
                        .expect("missing txo");

                    Some(Utxo {
                        txid: deserialize(&info.txid).expect("invalid txid"),
                        vout: info.vout as u32,
                        value: info.value,
                        confirmed: None,

                        #[cfg(feature = "liquid")]
                        asset: txo.asset,
                        #[cfg(feature = "liquid")]
                        nonce: txo.nonce,
                        #[cfg(feature = "liquid")]
                        witness: txo.witness,
                    })
                }
                TxHistoryInfo::Spending(_) => None,
                #[cfg(feature = "liquid")]
                TxHistoryInfo::Issuing(_)
                | TxHistoryInfo::Burning(_)
                | TxHistoryInfo::Pegin(_)
                | TxHistoryInfo::Pegout(_) => unreachable!(),
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
            if seen_txids.insert(entry.get_txid()) {
                stats.tx_count += 1;
            }

            match entry {
                #[cfg(not(feature = "liquid"))]
                TxHistoryInfo::Funding(info) => {
                    stats.funded_txo_count += 1;
                    stats.funded_txo_sum += info.value;
                }

                #[cfg(not(feature = "liquid"))]
                TxHistoryInfo::Spending(info) => {
                    stats.spent_txo_count += 1;
                    stats.spent_txo_sum += info.value;
                }

                // Elements
                #[cfg(feature = "liquid")]
                TxHistoryInfo::Funding(_) => {
                    stats.funded_txo_count += 1;
                }
                #[cfg(feature = "liquid")]
                TxHistoryInfo::Spending(_) => {
                    stats.spent_txo_count += 1;
                }
                #[cfg(feature = "liquid")]
                TxHistoryInfo::Issuing(_)
                | TxHistoryInfo::Burning(_)
                | TxHistoryInfo::Pegin(_)
                | TxHistoryInfo::Pegout(_) => unreachable!(),
            };
        }

        stats
    }

    // Get all txids in the mempool
    pub fn txids(&self) -> Vec<&Txid> {
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

    pub fn old_txids(&self) -> HashSet<Txid> {
        return HashSet::from_iter(self.txstore.keys().cloned());
    }

    pub fn update_backlog_stats(&mut self) {
        let _timer = self
            .latency
            .with_label_values(&["update_backlog_stats"])
            .start_timer();
        self.backlog_stats = (BacklogStats::new(&self.feeinfo), Instant::now());
    }

    pub fn add_by_txid(&mut self, daemon: &Daemon, txid: &Txid) {
        if self.txstore.get(txid).is_none() {
            if let Ok(tx) = daemon.getmempooltx(&txid) {
                self.add(vec![tx])
            }
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
            let prevouts = extract_tx_prevouts(&tx, &txos, false);

            // Get feeinfo for caching and recent tx overview
            let feeinfo = TxFeeInfo::new(&tx, &prevouts, self.config.network_type);

            // recent is an ArrayDeque that automatically evicts the oldest elements
            self.recent.push_front(TxOverview {
                txid,
                fee: feeinfo.fee,
                vsize: feeinfo.vsize,
                #[cfg(not(feature = "liquid"))]
                value: prevouts
                    .values()
                    .map(|prevout| prevout.value.to_sat())
                    .sum(),
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
                        value: prevout.value.amount_value(),
                    }),
                )
            });

            let config = &self.config;

            // An iterator over (ScriptHash, TxHistoryInfo)
            let funding = tx
                .output
                .iter()
                .enumerate()
                .filter(|(_, txo)| is_spendable(txo) || config.index_unspendables)
                .map(|(index, txo)| {
                    (
                        compute_script_hash(&txo.script_pubkey),
                        TxHistoryInfo::Funding(FundingInfo {
                            txid: txid_bytes,
                            vout: index as u16,
                            value: txo.value.amount_value(),
                        }),
                    )
                });

            // Index funding/spending history entries and spend edges
            for (scripthash, entry) in funding.chain(spending) {
                self.history
                    .entry(scripthash)
                    .or_insert_with(Vec::new)
                    .push(entry);
            }
            for (i, txi) in tx.input.iter().enumerate() {
                self.edges.insert(txi.previous_output, (txid, i as u32));
            }

            // Index issued assets & native asset pegins/pegouts/burns
            #[cfg(feature = "liquid")]
            asset::index_mempool_tx_assets(
                &tx,
                self.config.network_type,
                self.config.parent_network,
                &mut self.asset_history,
                &mut self.asset_issuance,
            );
        }
    }

    pub fn lookup_txo(&self, outpoint: &OutPoint) -> Result<TxOut> {
        let mut outpoints = BTreeSet::new();
        outpoints.insert(*outpoint);
        Ok(self.lookup_txos(&outpoints)?.remove(outpoint).unwrap())
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

    fn get_prevouts(&self, txids: &[Txid]) -> BTreeSet<OutPoint> {
        let _timer = self
            .latency
            .with_label_values(&["get_prevouts"])
            .start_timer();

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

    fn remove(&mut self, to_remove: HashSet<&Txid>) {
        self.delta
            .with_label_values(&["remove"])
            .observe(to_remove.len() as f64);
        let _timer = self.latency.with_label_values(&["remove"]).start_timer();

        for txid in &to_remove {
            self.txstore
                .remove(*txid)
                .unwrap_or_else(|| panic!("missing mempool tx {}", txid));

            self.feeinfo.remove(*txid).or_else(|| {
                warn!("missing mempool tx feeinfo {}", txid);
                None
            });
        }

        // TODO: make it more efficient (currently it takes O(|mempool|) time)
        self.history.retain(|_scripthash, entries| {
            entries.retain(|entry| !to_remove.contains(&entry.get_txid()));
            !entries.is_empty()
        });

        #[cfg(feature = "liquid")]
        asset::remove_mempool_tx_assets(
            &to_remove,
            &mut self.asset_history,
            &mut self.asset_issuance,
        );

        self.edges
            .retain(|_outpoint, (txid, _vin)| !to_remove.contains(txid));
    }

    #[cfg(feature = "liquid")]
    pub fn asset_history(&self, asset_id: &AssetId, limit: usize) -> Vec<Transaction> {
        let _timer = self
            .latency
            .with_label_values(&["asset_history"])
            .start_timer();
        self.asset_history
            .get(asset_id)
            .map_or_else(|| vec![], |entries| self._history(entries, limit))
    }

    pub fn update(mempool: &Arc<RwLock<Mempool>>, daemon: &Daemon) -> Result<()> {
        let _timer = mempool.read().unwrap().latency.with_label_values(&["update"]).start_timer();

        // 1. Determine which transactions are no longer in the daemon's mempool and which ones have newly entered it
        let old_txids = mempool.read().unwrap().old_txids();
        let all_txids = daemon
            .getmempooltxids()
            .chain_err(|| "failed to update mempool from daemon")?;
        let txids_to_remove: HashSet<&Txid> = old_txids.difference(&all_txids).collect();

        // 2. Remove missing transactions. Even if we are unable to download new transactions from
        // the daemon, we still want to remove the transactions that are no longer in the mempool.
        mempool.write().unwrap().remove(txids_to_remove);

        // 3. Download the new transactions from the daemon's mempool
        let new_txids: Vec<&Txid> = all_txids.difference(&old_txids).collect();
        let txs_to_add = daemon
            .gettransactions(&new_txids)
            .chain_err(|| format!("failed to get {} transactions", new_txids.len()))?;

        // 4. Update local mempool to match daemon's state
        {
            let mut mempool = mempool.write().unwrap();
            // Add new transactions
            mempool.add(txs_to_add);

            mempool
                .count
                .with_label_values(&["txs"])
                .set(mempool.txstore.len() as f64);

            // Update cached backlog stats (if expired)
            if mempool.backlog_stats.1.elapsed() > Duration::from_secs(BACKLOG_STATS_TTL) {
                mempool.update_backlog_stats();
            }
        }

        Ok(())
    }
}

#[derive(Serialize)]
pub struct BacklogStats {
    pub count: u32,
    pub vsize: u64,     // in virtual bytes (= weight/4)
    pub total_fee: u64, // in satoshis
    pub fee_histogram: Vec<(f64, u64)>,
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

    fn new(feeinfo: &HashMap<Txid, TxFeeInfo>) -> Self {
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
