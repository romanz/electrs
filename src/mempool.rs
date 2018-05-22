use bitcoin::blockdata::transaction::Transaction;
use bitcoin::util::hash::Sha256dHash;
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};
use std::iter::FromIterator;
use std::ops::Bound;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use daemon::{Daemon, MempoolEntry};
use index::index_transaction;
use store::{ReadStore, Row};
use util::Bytes;

error_chain!{}

const VSIZE_BIN_WIDTH: u32 = 100_000; // in vbytes

struct MempoolStore {
    map: RwLock<BTreeMap<Bytes, Bytes>>,
}

impl MempoolStore {
    fn new(rows: Vec<Row>) -> MempoolStore {
        let mut map = BTreeMap::new();
        for row in rows {
            let (key, value) = row.into_pair();
            map.insert(key, value);
        }
        MempoolStore {
            map: RwLock::new(map),
        }
    }
}

impl ReadStore for MempoolStore {
    fn get(&self, key: &[u8]) -> Option<Bytes> {
        self.map.read().unwrap().get(key).map(|v| v.to_vec())
    }
    fn scan(&self, prefix: &[u8]) -> Vec<Row> {
        let map = self.map.read().unwrap();
        let range = map.range((Bound::Included(prefix.to_vec()), Bound::Unbounded));
        let mut rows = Vec::new();
        for (key, value) in range {
            if !key.starts_with(prefix) {
                break;
            }
            rows.push(Row {
                key: key.to_vec(),
                value: value.to_vec(),
            });
        }
        rows
    }
}

pub struct Stats {
    tx: Transaction,
    entry: MempoolEntry,
}

impl Stats {
    pub fn new(tx: Transaction, entry: MempoolEntry) -> Stats {
        Stats { tx, entry }
    }
}

pub struct Tracker {
    stats: HashMap<Sha256dHash, Stats>,
    index: MempoolStore,
    histogram: Vec<(f32, u32)>,
}

impl Tracker {
    pub fn new() -> Tracker {
        Tracker {
            stats: HashMap::new(),
            index: MempoolStore::new(vec![]),
            histogram: vec![],
        }
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
        let t = Instant::now();
        let new_txids = HashSet::<Sha256dHash>::from_iter(daemon
            .getmempooltxids()
            .chain_err(|| "failed to update mempool from daemon")?);
        let old_txids = HashSet::from_iter(self.stats.keys().cloned());
        for txid in new_txids.difference(&old_txids) {
            let entry = match daemon.getmempoolentry(txid) {
                Ok(entry) => entry,
                Err(err) => {
                    // e.g. new block or RBF
                    warn!("no mempool entry {}: {}", txid, err);
                    continue;
                }
            };
            let tx = match daemon.gettransaction(txid) {
                Ok(tx) => tx,
                Err(err) => {
                    // e.g. new block or RBF
                    warn!("missing tx {}: {}", txid, err);
                    continue;
                }
            };
            trace!("new tx: {}, {:.3}", txid, entry.fee_per_vbyte(),);
            self.add(txid, tx, entry);
        }
        for txid in old_txids.difference(&new_txids) {
            self.remove(txid);
        }
        self.update_tx_index();
        self.update_fee_histogram();
        debug!(
            "mempool update took {:.1} ms ({} txns)",
            t.elapsed().in_seconds() * 1e3,
            self.stats.len()
        );
        Ok(())
    }

    fn add(&mut self, txid: &Sha256dHash, tx: Transaction, entry: MempoolEntry) {
        self.stats.insert(*txid, Stats { tx, entry });
    }

    fn remove(&mut self, txid: &Sha256dHash) {
        self.stats.remove(txid);
    }

    fn update_tx_index(&mut self) {
        let mut rows = Vec::new();
        for stats in self.stats.values() {
            index_transaction(&stats.tx, 0, &mut rows)
        }
        self.index = MempoolStore::new(rows);
    }

    fn update_fee_histogram(&mut self) {
        let mut entries: Vec<&MempoolEntry> = self.stats.values().map(|stat| &stat.entry).collect();
        entries.sort_unstable_by(|e1, e2| {
            e2.fee_per_vbyte().partial_cmp(&e1.fee_per_vbyte()).unwrap()
        });
        let mut histogram = Vec::new();
        let mut bin_size = 0;
        let mut last_fee_rate = None;
        for e in entries {
            last_fee_rate = Some(e.fee_per_vbyte());
            bin_size += e.vsize();
            if bin_size > VSIZE_BIN_WIDTH {
                histogram.push((e.fee_per_vbyte(), bin_size));
                bin_size = 0;
            }
        }
        if let Some(fee_rate) = last_fee_rate {
            histogram.push((fee_rate, bin_size));
        }
        self.histogram = histogram;
    }
}

trait InSeconds {
    fn in_seconds(&self) -> f64;
}

impl InSeconds for Duration {
    fn in_seconds(&self) -> f64 {
        self.as_secs() as f64 + (self.subsec_nanos() as f64) * 1e-9
    }
}
