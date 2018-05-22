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
use store::{Row, Store};
use util::Bytes;

error_chain!{}

const VSIZE_BIN_WIDTH: u32 = 100_000; // in vbytes

struct MempoolStore {
    map: RwLock<BTreeMap<Bytes, Bytes>>,
}

impl MempoolStore {
    fn new() -> MempoolStore {
        MempoolStore {
            map: RwLock::new(BTreeMap::new()),
        }
    }

    fn clear(&self) {
        self.map.write().unwrap().clear()
    }
}

impl Store for MempoolStore {
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
    fn persist(&self, rows: Vec<Row>) {
        let mut map = self.map.write().unwrap();
        for row in rows {
            let (key, value) = row.into_pair();
            map.insert(key, value);
        }
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
}

impl Tracker {
    pub fn new() -> Tracker {
        Tracker {
            stats: HashMap::new(),
            index: MempoolStore::new(),
        }
    }

    /// Returns vector of (fee_rate, vsize) pairs, where fee_{n-1} > fee_n and vsize_n is the
    /// cumulative virtual size of mempool transaction with fee in the interval [fee_{n-1}, fee_n].
    /// Note: fee_0 is implied to be infinity.
    pub fn fee_histogram(&self) -> Vec<(f32, u32)> {
        let mut entries: Vec<&MempoolEntry> = self.stats.values().map(|stat| &stat.entry).collect();
        entries.sort_unstable_by(|e1, e2| {
            e2.fee_per_vbyte().partial_cmp(&e1.fee_per_vbyte()).unwrap()
        });
        let mut histogram = Vec::new();
        let mut cumulative_vsize = 0;
        for e in entries {
            cumulative_vsize += e.vsize();
            if cumulative_vsize > VSIZE_BIN_WIDTH {
                histogram.push((e.fee_per_vbyte(), cumulative_vsize));
                cumulative_vsize = 0;
            }
        }
        histogram.push((0.0, cumulative_vsize));
        histogram
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

        let mut rows = Vec::new();
        for stats in self.stats.values() {
            index_transaction(&stats.tx, 0, &mut rows)
        }
        self.index.clear();
        self.index.persist(rows);

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

    pub fn index(&self) -> &Store {
        &self.index
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
