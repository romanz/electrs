use bitcoin::blockdata::transaction::Transaction;
use bitcoin::util::hash::Sha256dHash;
use daemon::{Daemon, MempoolEntry};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::iter::FromIterator;

error_chain!{}

const VSIZE_BIN_WIDTH: u32 = 100_000; // in vbytes

pub struct Stats {
    tx: Transaction,
    entry: MempoolEntry,
}

pub struct Tracker<'a> {
    txids: HashMap<Sha256dHash, Stats>,
    daemon: &'a Daemon,
}

impl<'a> Tracker<'a> {
    pub fn new(daemon: &Daemon) -> Tracker {
        Tracker {
            txids: HashMap::new(),
            daemon: daemon,
        }
    }

    /// Returns vector of (fee_rate, vsize) pairs, where fee_{n-1} > fee_n and vsize_n is the
    /// cumulative virtual size of mempool transaction with fee in the interval [fee_{n-1}, fee_n].
    /// Note: fee_0 is implied to be infinity.
    pub fn fee_histogram(&self) -> Vec<(f32, u32)> {
        let mut entries: Vec<&MempoolEntry> = self.txids.values().map(|stat| &stat.entry).collect();
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

    pub fn update_from_daemon(&mut self) -> Result<()> {
        let new_txids = HashSet::<Sha256dHash>::from_iter(self.daemon
            .getmempooltxids()
            .chain_err(|| "failed to update mempool from daemon")?);
        self.txids.retain(|txid, _| new_txids.contains(txid)); // remove old TXNs
        for txid in new_txids {
            if let Entry::Vacant(map_entry) = self.txids.entry(txid) {
                let tx = match self.daemon.gettransaction(&txid) {
                    Ok(tx) => tx,
                    Err(err) => {
                        warn!("missing tx {}: {}", txid, err);
                        continue;
                    }
                };
                let entry = match self.daemon.getmempoolentry(&txid) {
                    Ok(entry) => entry,
                    Err(err) => {
                        warn!("no mempool entry {}: {}", txid, err);
                        continue;
                    }
                };
                debug!("new tx: {}, {:.3}", txid, entry.fee_per_vbyte(),);
                map_entry.insert(Stats { tx, entry });
            }
        }
        Ok(())
    }
}
