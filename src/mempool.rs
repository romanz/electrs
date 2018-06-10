use bitcoin::blockdata::transaction::Transaction;
use bitcoin::util::hash::Sha256dHash;
use hex;
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};
use std::iter::FromIterator;
use std::ops::Bound;
use std::sync::RwLock;

use daemon::{Daemon, MempoolEntry};
use index::index_transaction;
use store::{ReadStore, Row};
use util::{Bytes, Timer};

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

pub struct Stats {
    tx: Transaction,
    entry: MempoolEntry,
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
            index: MempoolStore::new(),
            histogram: vec![],
        }
    }

    pub fn get_txn(&self, txid: &Sha256dHash) -> Option<Transaction> {
        self.stats.get(txid).map(|stats| stats.tx.clone())
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
        let mut metric = Timer::new();
        let new_txids = daemon
            .getmempooltxids()
            .chain_err(|| "failed to update mempool from daemon")?;
        let old_txids = HashSet::from_iter(self.stats.keys().cloned());
        metric.tick("fetch");
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
        for txid in old_txids.difference(&new_txids) {
            self.remove(txid);
        }
        metric.tick("index");
        self.update_fee_histogram();
        metric.tick("fees");
        debug!("mempool update ({} txns) {:?}", self.stats.len(), metric);
        Ok(())
    }

    fn add(&mut self, txid: &Sha256dHash, tx: Transaction, entry: MempoolEntry) {
        trace!("new tx: {}, {:.3}", txid, entry.fee_per_vbyte());
        self.index.add(&tx);
        self.stats.insert(*txid, Stats { tx, entry });
    }

    fn remove(&mut self, txid: &Sha256dHash) {
        let stats = self.stats
            .remove(txid)
            .expect(&format!("missing mempool tx {}", txid));
        self.index.remove(&stats.tx);
    }

    fn update_fee_histogram(&mut self) {
        let mut entries: Vec<&MempoolEntry> = self.stats.values().map(|stat| &stat.entry).collect();
        entries.sort_unstable_by(|e1, e2| {
            e2.fee_per_vbyte().partial_cmp(&e1.fee_per_vbyte()).unwrap()
        });
        let mut histogram = vec![];
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
