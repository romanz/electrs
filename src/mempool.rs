use anyhow::Result;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::convert::TryFrom;
use std::iter::FromIterator;
use std::ops::Bound;

use bitcoin::hashes::Hash;
use bitcoin::{Amount, OutPoint, Transaction, Txid};
use bitcoincore_rpc::json;
use rayon::prelude::*;
use serde::ser::{Serialize, SerializeSeq, Serializer};

use crate::{daemon::Daemon, types::ScriptHash};

pub(crate) struct Entry {
    pub txid: Txid,
    pub tx: Transaction,
    pub fee: Amount,
    pub vsize: u64,
    pub has_unconfirmed_inputs: bool,
}

/// Mempool current state
pub(crate) struct Mempool {
    entries: HashMap<Txid, Entry>,
    by_funding: BTreeSet<(ScriptHash, Txid)>,
    by_spending: BTreeSet<(OutPoint, Txid)>,
    histogram: Histogram,
}

// Smallest possible txid
fn txid_min() -> Txid {
    Txid::from_inner([0x00; 32])
}

// Largest possible txid
fn txid_max() -> Txid {
    Txid::from_inner([0xFF; 32])
}

impl Mempool {
    pub fn new() -> Self {
        Self {
            entries: Default::default(),
            by_funding: Default::default(),
            by_spending: Default::default(),
            histogram: Histogram::empty(),
        }
    }

    pub(crate) fn fees_histogram(&self) -> &Histogram {
        &self.histogram
    }

    pub(crate) fn get(&self, txid: &Txid) -> Option<&Entry> {
        self.entries.get(txid)
    }

    pub(crate) fn filter_by_funding(&self, scripthash: &ScriptHash) -> Vec<&Entry> {
        let range = (
            Bound::Included((*scripthash, txid_min())),
            Bound::Included((*scripthash, txid_max())),
        );
        self.by_funding
            .range(range)
            .map(|(_, txid)| self.get(txid).expect("missing funding mempool tx"))
            .collect()
    }

    pub(crate) fn filter_by_spending(&self, outpoint: &OutPoint) -> Vec<&Entry> {
        let range = (
            Bound::Included((*outpoint, txid_min())),
            Bound::Included((*outpoint, txid_max())),
        );
        self.by_spending
            .range(range)
            .map(|(_, txid)| self.get(txid).expect("missing spending mempool tx"))
            .collect()
    }

    pub fn sync(&mut self, daemon: &Daemon) {
        let txids = match daemon.get_mempool_txids() {
            Ok(txids) => txids,
            Err(e) => {
                warn!("mempool sync failed: {}", e);
                return;
            }
        };
        debug!("loading {} mempool transactions", txids.len());

        let new_txids = HashSet::<Txid>::from_iter(txids);
        let old_txids = HashSet::<Txid>::from_iter(self.entries.keys().copied());

        let to_add = &new_txids - &old_txids;
        let to_remove = &old_txids - &new_txids;

        let removed = to_remove.len();
        for txid in to_remove {
            self.remove_entry(txid);
        }
        let entries: Vec<_> = to_add
            .par_iter()
            .filter_map(|txid| {
                match (
                    daemon.get_transaction(txid, None),
                    daemon.get_mempool_entry(txid),
                ) {
                    (Ok(tx), Ok(entry)) => Some((txid, tx, entry)),
                    _ => None,
                }
            })
            .collect();
        let added = entries.len();
        for (txid, tx, entry) in entries {
            self.add_entry(*txid, tx, entry);
        }
        self.histogram = Histogram::new(self.entries.values().map(|e| (e.fee, e.vsize)));
        debug!(
            "{} mempool txs: {} added, {} removed",
            self.entries.len(),
            added,
            removed,
        );
    }

    fn add_entry(&mut self, txid: Txid, tx: Transaction, entry: json::GetMempoolEntryResult) {
        for txi in &tx.input {
            self.by_spending.insert((txi.previous_output, txid));
        }
        for txo in &tx.output {
            let scripthash = ScriptHash::new(&txo.script_pubkey);
            self.by_funding.insert((scripthash, txid)); // may have duplicates
        }
        let entry = Entry {
            txid,
            tx,
            vsize: entry.vsize,
            fee: entry.fees.base,
            has_unconfirmed_inputs: !entry.depends.is_empty(),
        };
        assert!(
            self.entries.insert(txid, entry).is_none(),
            "duplicate mempool txid"
        );
    }

    fn remove_entry(&mut self, txid: Txid) {
        let entry = self.entries.remove(&txid).expect("missing tx from mempool");
        for txi in entry.tx.input {
            self.by_spending.remove(&(txi.previous_output, txid));
        }
        for txo in entry.tx.output {
            let scripthash = ScriptHash::new(&txo.script_pubkey);
            self.by_funding.remove(&(scripthash, txid)); // may have misses
        }
    }
}

pub(crate) struct Histogram {
    /// bins[64-i] contains the total vsize of transactions with fee rate [2**(i-1), 2**i).
    /// bins[63] = [1, 2)
    /// bins[62] = [2, 4)
    /// bins[61] = [4, 8)
    /// bins[60] = [8, 16)
    /// ...
    /// bins[1] = [2**62, 2**63)
    /// bins[0] = [2**63, 2**64)
    bins: [u64; Histogram::SIZE],
}

impl Histogram {
    const SIZE: usize = 64;

    fn empty() -> Self {
        Self::new(std::iter::empty())
    }

    fn new(items: impl Iterator<Item = (Amount, u64)>) -> Self {
        let mut bins = [0; Self::SIZE];
        for (fee, vsize) in items {
            let fee_rate = fee.as_sat() / vsize;
            let index = usize::try_from(fee_rate.leading_zeros()).unwrap();
            // skip transactions with too low fee rate (<1 sat/vB)
            if let Some(bin) = bins.get_mut(index) {
                *bin += vsize
            }
        }
        Self { bins }
    }
}

impl Serialize for Histogram {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(self.bins.len()))?;
        // https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-methods.html#mempool-get-fee-histogram
        let fee_rates = (0..Histogram::SIZE).map(|i| std::u64::MAX >> i);
        fee_rates
            .zip(self.bins.iter().copied())
            .skip_while(|(_fee_rate, vsize)| *vsize == 0)
            .try_for_each(|element| seq.serialize_element(&element))?;
        seq.end()
    }
}

#[cfg(test)]
mod tests {
    use super::Histogram;
    use bitcoin::Amount;
    use serde_json::json;

    #[test]
    fn test_histogram() {
        let items = vec![
            (Amount::from_sat(20), 10),
            (Amount::from_sat(10), 10),
            (Amount::from_sat(60), 10),
            (Amount::from_sat(30), 10),
            (Amount::from_sat(70), 10),
            (Amount::from_sat(50), 10),
            (Amount::from_sat(40), 10),
            (Amount::from_sat(80), 10),
        ];
        let hist = json!(Histogram::new(items.into_iter()));
        assert_eq!(hist, json!([[15, 10], [7, 40], [3, 20], [1, 10]]));
    }
}
