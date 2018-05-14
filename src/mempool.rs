use bitcoin::blockdata::transaction::Transaction;
use bitcoin::util::hash::Sha256dHash;
use daemon::{Daemon, MempoolEntry};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::iter::FromIterator;

use query::{FundingOutput, SpendingInput};
use types::FullHash;

error_chain!{}

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
                debug!("new mempool tx: {}, {:.3}", txid, entry.fee_per_byte());
                map_entry.insert(Stats { tx, entry });
            }
        }
        Ok(())
    }
}
