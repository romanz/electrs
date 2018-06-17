use bitcoin::blockdata::transaction::Transaction;
use bitcoin::util::hash::Sha256dHash;
use hex;
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};
use std::iter::FromIterator;
use std::ops::Bound;
use std::sync::{Mutex, RwLock};

use daemon::{Daemon, MempoolEntry};
use index::index_transaction;
use metrics::{Gauge, GaugeVec, HistogramOpts, HistogramTimer, HistogramVec, MetricOpts, Metrics};
use store::{ReadStore, Row};
use util::Bytes;

use errors::*;

const VSIZE_BIN_WIDTH: u32 = 100_000; // in vbytes

struct MempoolStore {
    map: RwLock<BTreeMap<Bytes, Vec<Bytes>>>,
}

impl MempoolStore {
    fn new() -> MempoolStore {
        MempoolStore {
            map: RwLock::new(BTreeMap::new()),
        }
    }

    fn add(&mut self, tx: &Transaction) {
        let mut map = self.map.write().unwrap();
        let mut rows = vec![];
        index_transaction(tx, 0, &mut rows);
        for row in rows {
            let (key, value) = row.into_pair();
            map.entry(key).or_insert(vec![]).push(value);
        }
    }

    fn remove(&mut self, tx: &Transaction) {
        let mut map = self.map.write().unwrap();
        let mut rows = vec![];
        index_transaction(tx, 0, &mut rows);
        for row in rows {
            let (key, value) = row.into_pair();
            let no_values_left = {
                let values = map.get_mut(&key)
                    .expect(&format!("missing key {} in mempool", hex::encode(&key)));
                let last_value = values
                    .pop()
                    .expect(&format!("no values found for key {}", hex::encode(&key)));
                // TxInRow and TxOutRow have an empty value, TxRow has height=0 as value.
                assert_eq!(
                    value,
                    last_value,
                    "wrong value for key {}: {}",
                    hex::encode(&key),
                    hex::encode(&last_value)
                );
                values.is_empty()
            };
            if no_values_left {
                map.remove(&key).unwrap();
            }
        }
    }
}

impl ReadStore for MempoolStore {
    fn get(&self, key: &[u8]) -> Option<Bytes> {
        let map = self.map.read().unwrap();
        Some(map.get(key)?.last()?.to_vec())
    }
    fn scan(&self, prefix: &[u8]) -> Vec<Row> {
        let map = self.map.read().unwrap();
        let range = map.range((Bound::Included(prefix.to_vec()), Bound::Unbounded));
        let mut rows = vec![];
        for (key, values) in range {
            if !key.starts_with(prefix) {
                break;
            }
            if let Some(value) = values.last() {
                rows.push(Row {
                    key: key.to_vec(),
                    value: value.to_vec(),
                });
            }
        }
        rows
    }
}

struct Item {
    tx: Transaction,
    entry: MempoolEntry,
}

struct Stats {
    count: Gauge,
    vsize: Gauge,
    update: HistogramVec,
    fees: GaugeVec,
    max_fee_rate: Mutex<f32>,
}

impl Stats {
    fn start_timer(&self, step: &str) -> HistogramTimer {
        self.update.with_label_values(&[step]).start_timer()
    }

    fn update_fees(&self, entries: &[&MempoolEntry]) {
        let mut bands: Vec<(f32, u32)> = vec![];
        let mut fee_rate = 1.0f32; // [sat/vbyte]
        let mut vsize = 0u32; // vsize of transactions paying <= fee_rate
        for e in entries {
            while fee_rate < e.fee_per_vbyte() {
                bands.push((fee_rate, vsize));
                fee_rate *= 2.0;
            }
            vsize += e.vsize();
        }
        let mut max_fee_rate = self.max_fee_rate.lock().unwrap();
        loop {
            bands.push((fee_rate, vsize));
            if fee_rate < *max_fee_rate {
                fee_rate *= 2.0;
                continue;
            }
            *max_fee_rate = fee_rate;
            break;
        }
        drop(max_fee_rate);
        for (fee_rate, vsize) in bands {
            // labels should be ordered by fee_rate value
            let label = format!("≤{:10.0}", fee_rate);
            self.fees.with_label_values(&[&label]).set(vsize as f64);
        }
    }
}

pub struct Tracker {
    items: HashMap<Sha256dHash, Item>,
    index: MempoolStore,
    histogram: Vec<(f32, u32)>,
    stats: Stats,
}

impl Tracker {
    pub fn new(metrics: &Metrics) -> Tracker {
        Tracker {
            items: HashMap::new(),
            index: MempoolStore::new(),
            histogram: vec![],
            stats: Stats {
                count: metrics.gauge(MetricOpts::new(
                    "mempool_count",
                    "# of mempool transactions",
                )),
                vsize: metrics.gauge(MetricOpts::new(
                    "mempool_vsize",
                    "vsize of mempool transactions (in bytes)",
                )),
                update: metrics.histogram_vec(
                    HistogramOpts::new("mempool_update", "Time to update mempool (in seconds)"),
                    &["step"],
                ),
                fees: metrics.gauge_vec(
                    MetricOpts::new(
                        "mempool_fees",
                        "Total vsize of transactions paying at most given fee rate",
                    ),
                    &["fee_rate"],
                ),
                max_fee_rate: Mutex::new(1.0),
            },
        }
    }

    pub fn get_txn(&self, txid: &Sha256dHash) -> Option<Transaction> {
        self.items.get(txid).map(|stats| stats.tx.clone())
    }

    /// Returns vector of (fee_rate, vsize) pairs, where fee_{n-1} > fee_n and vsize_n is the
    /// total virtual size of mempool transactions with fee in the bin [fee_{n-1}, fee_n].
    /// Note: fee_{-1} is implied to be infinite.
    pub fn fee_histogram(&self) -> &Vec<(f32, u32)> {
        &self.histogram
    }

    pub fn index(&self) -> &ReadStore {
        &self.index
    }

    pub fn update(&mut self, daemon: &Daemon) -> Result<()> {
        let timer = self.stats.start_timer("fetch");
        let new_txids = daemon
            .getmempooltxids()
            .chain_err(|| "failed to update mempool from daemon")?;
        let old_txids = HashSet::from_iter(self.items.keys().cloned());
        timer.observe_duration();

        let timer = self.stats.start_timer("add");
        for txid in new_txids.difference(&old_txids) {
            let entry = match daemon.getmempoolentry(txid) {
                Ok(entry) => entry,
                Err(err) => {
                    warn!("no mempool entry {}: {}", txid, err); // e.g. new block or RBF
                    continue;
                }
            };
            // The following lookup should find the transaction in mempool.
            let tx = match daemon.gettransaction(txid, /*blockhash=*/ None) {
                Ok(tx) => tx,
                Err(err) => {
                    warn!("missing tx {}: {}", txid, err); // e.g. new block or RBF
                    continue;
                }
            };
            self.add(txid, tx, entry);
        }
        timer.observe_duration();

        let timer = self.stats.start_timer("remove");
        for txid in old_txids.difference(&new_txids) {
            self.remove(txid);
        }
        timer.observe_duration();

        let timer = self.stats.start_timer("fees");
        self.update_fee_histogram();
        timer.observe_duration();

        self.stats.count.set(self.items.len() as i64);
        self.stats.vsize.set(
            self.items
                .values()
                .map(|stat| stat.entry.vsize() as i64)
                .sum::<i64>(),
        );
        Ok(())
    }

    fn add(&mut self, txid: &Sha256dHash, tx: Transaction, entry: MempoolEntry) {
        trace!("new tx: {}, {:.3}", txid, entry.fee_per_vbyte());
        self.index.add(&tx);
        self.items.insert(*txid, Item { tx, entry });
    }

    fn remove(&mut self, txid: &Sha256dHash) {
        let stats = self.items
            .remove(txid)
            .expect(&format!("missing mempool tx {}", txid));
        self.index.remove(&stats.tx);
    }

    fn update_fee_histogram(&mut self) {
        let mut entries: Vec<&MempoolEntry> = self.items.values().map(|stat| &stat.entry).collect();
        entries.sort_unstable_by(|e1, e2| {
            e1.fee_per_vbyte().partial_cmp(&e2.fee_per_vbyte()).unwrap()
        });
        self.histogram = electrum_fees(&entries);
        self.stats.update_fees(&entries);
    }
}

fn electrum_fees(entries: &[&MempoolEntry]) -> Vec<(f32, u32)> {
    let mut histogram = vec![];
    let mut bin_size = 0;
    let mut last_fee_rate = None;
    for e in entries.iter().rev() {
        last_fee_rate = Some(e.fee_per_vbyte());
        bin_size += e.vsize();
        if bin_size > VSIZE_BIN_WIDTH {
            // vsize of transactions paying >= e.fee_per_vbyte()
            histogram.push((e.fee_per_vbyte(), bin_size));
            bin_size = 0;
        }
    }
    if let Some(fee_rate) = last_fee_rate {
        histogram.push((fee_rate, bin_size));
    }
    histogram
}
