use bitcoin::blockdata::transaction::Transaction;
use bitcoin::util::hash::Sha256dHash;
use daemon::{Daemon, MempoolEntry};

use std::collections::{HashMap, HashSet};
use std::iter::FromIterator;
use std::time::{Duration, Instant};

error_chain!{}

const VSIZE_BIN_WIDTH: u32 = 100_000; // in vbytes

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
}

impl Tracker {
    pub fn new() -> Tracker {
        Tracker {
            stats: HashMap::new(),
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
        let new_txids = HashSet::<Sha256dHash>::from_iter(daemon
            .getmempooltxids()
            .chain_err(|| "failed to update mempool from daemon")?);
        let old_txids = HashSet::from_iter(self.stats.keys().cloned());
        let t = Instant::now();
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
            self.add(txid, Stats::new(tx, entry));
        }
        for txid in old_txids.difference(&new_txids) {
            self.remove(txid);
        }
        let dt = t.elapsed();
        debug!(
            "mempool update took {:.1} ms ({} txns)",
            dt.in_seconds() * 1e3,
            self.stats.len()
        );
        Ok(())
    }

    fn add(&mut self, txid: &Sha256dHash, stats: Stats) {
        self.stats.insert(*txid, stats);
    }

    fn remove(&mut self, txid: &Sha256dHash) {
        self.stats.remove(txid);
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
