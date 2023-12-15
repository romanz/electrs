use anyhow::{Context, Result};

use std::collections::{BTreeSet, HashMap, HashSet};
use std::convert::TryFrom;
use std::iter::FromIterator;
use std::ops::Bound;

use bitcoin::hashes::Hash;
use bitcoin::{Amount, OutPoint, Transaction, Txid};
use serde::ser::{Serialize, SerializeSeq, Serializer};

use crate::{
    daemon::Daemon,
    metrics::{Gauge, Metrics},
    signals::ExitFlag,
    types::ScriptHash,
};

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
    fees: FeeHistogram,
    // stats
    vsize: Gauge,
    count: Gauge,
}

/// An update to [`Mempool`]'s internal state. This can be fetched
/// asynchronously using [`MempoolSyncUpdate::poll`], and applied
/// using [`Mempool::apply_sync_update`].
pub(crate) struct MempoolSyncUpdate {
    new_entries: Vec<Entry>,
    removed_entries: HashSet<Txid>,
}

impl MempoolSyncUpdate {
    /// Poll the bitcoin node and compute a [`MempoolSyncUpdate`] based on the given set of
    /// `old_txids` which are already cached.
    pub fn poll(
        daemon: &Daemon,
        old_txids: HashSet<Txid>,
        exit_flag: &ExitFlag,
    ) -> Result<MempoolSyncUpdate> {
        let txids = daemon.get_mempool_txids()?;
        debug!("loading {} mempool transactions", txids.len());

        let new_txids = HashSet::<Txid>::from_iter(txids);

        let to_add = &new_txids - &old_txids;
        let to_remove = &old_txids - &new_txids;

        let to_add: Vec<Txid> = to_add.into_iter().collect();
        let mut new_entries = Vec::with_capacity(to_add.len());

        for txids_chunk in to_add.chunks(1000) {
            exit_flag.poll().context("mempool update interrupted")?;
            let entries = daemon.get_mempool_entries(txids_chunk)?;
            ensure!(
                txids_chunk.len() == entries.len(),
                "got {} mempools entries, expected {}",
                entries.len(),
                txids_chunk.len()
            );
            let txs = daemon.get_mempool_transactions(txids_chunk)?;
            ensure!(
                txids_chunk.len() == txs.len(),
                "got {} mempools transactions, expected {}",
                txs.len(),
                txids_chunk.len()
            );
            let chunk_entries: Vec<Entry> = txids_chunk
                .iter()
                .zip(entries.into_iter().zip(txs.into_iter()))
                .filter_map(|(txid, (entry, tx))| {
                    let tx = tx.ok()?;
                    let entry = entry.ok()?;
                    Some(Entry {
                        txid: *txid,
                        tx,
                        vsize: entry.vsize,
                        fee: entry.fees.base,
                        has_unconfirmed_inputs: !entry.depends.is_empty(),
                    })
                })
                .collect();

            new_entries.extend(chunk_entries);
        }

        let update = MempoolSyncUpdate {
            new_entries,
            removed_entries: to_remove,
        };
        Ok(update)
    }
}

// Smallest possible txid
fn txid_min() -> Txid {
    Txid::all_zeros()
}

// Largest possible txid
fn txid_max() -> Txid {
    Txid::from_byte_array([0xFF; 32])
}

impl Mempool {
    pub fn new(metrics: &Metrics) -> Self {
        Self {
            entries: Default::default(),
            by_funding: Default::default(),
            by_spending: Default::default(),
            fees: FeeHistogram::default(),
            vsize: metrics.gauge(
                "mempool_txs_vsize",
                "Total vsize of mempool transactions (in bytes)",
                "fee_rate",
            ),
            count: metrics.gauge(
                "mempool_txs_count",
                "Total number of mempool transactions",
                "fee_rate",
            ),
        }
    }

    pub(crate) fn fees_histogram(&self) -> &FeeHistogram {
        &self.fees
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

    /// Apply a [`MempoolSyncUpdate`] to the mempool state.
    pub fn apply_sync_update(&mut self, update: MempoolSyncUpdate) {
        let removed = update.removed_entries.len();
        let added = update.new_entries.len();

        for txid_to_remove in update.removed_entries {
            self.remove_entry(txid_to_remove);
        }

        for entry in update.new_entries {
            self.add_entry(entry);
        }

        self.update_metrics();

        debug!(
            "{} mempool txs: {} added, {} removed",
            self.entries.len(),
            added,
            removed,
        );
    }

    fn update_metrics(&mut self) {
        for i in 0..FeeHistogram::BINS {
            let bin_index = FeeHistogram::BINS - i - 1; // from 63 to 0
            let (lower, upper) = FeeHistogram::bin_range(bin_index);
            let label = format!("[{:20.0}, {:20.0})", lower, upper);
            self.vsize.set(&label, self.fees.vsize[bin_index] as f64);
            self.count.set(&label, self.fees.count[bin_index] as f64);
        }
    }

    pub fn sync(&mut self, daemon: &Daemon, exit_flag: &ExitFlag) {
        let old_txids = HashSet::<Txid>::from_iter(self.entries.keys().copied());

        let poll_result = MempoolSyncUpdate::poll(daemon, old_txids, exit_flag);

        let sync_update = match poll_result {
            Ok(sync_update) => sync_update,
            Err(e) => {
                warn!("mempool sync failed: {}", e);
                return;
            }
        };

        self.apply_sync_update(sync_update);
    }

    /// Add a transaction entry to the mempool and update the fee histogram.
    fn add_entry(&mut self, entry: Entry) {
        for txi in &entry.tx.input {
            self.by_spending.insert((txi.previous_output, entry.txid));
        }
        for txo in &entry.tx.output {
            let scripthash = ScriptHash::new(&txo.script_pubkey);
            self.by_funding.insert((scripthash, entry.txid)); // may have duplicates
        }

        self.modify_fee_histogram(entry.fee, entry.vsize as i64);

        assert!(
            self.entries.insert(entry.txid, entry).is_none(),
            "duplicate mempool txid"
        );
    }

    /// Remove a transaction entry from the mempool and update the fee histogram.
    fn remove_entry(&mut self, txid: Txid) {
        let entry = self.entries.remove(&txid).expect("missing tx from mempool");
        for txi in entry.tx.input {
            self.by_spending.remove(&(txi.previous_output, txid));
        }
        for txo in entry.tx.output {
            let scripthash = ScriptHash::new(&txo.script_pubkey);
            self.by_funding.remove(&(scripthash, txid)); // may have misses
        }

        self.modify_fee_histogram(entry.fee, -(entry.vsize as i64));
    }

    /// Apply a change to the fee histogram. Used when transactions are added or
    /// removed from the mempool. If `vsize_change` is positive, we increase
    /// the histogram vsize and TX count in the appropriate bin. If negative,
    /// we decrease them.
    fn modify_fee_histogram(&mut self, fee: Amount, vsize_change: i64) {
        let vsize = vsize_change.unsigned_abs();
        let bin_index = FeeHistogram::bin_index(fee, vsize);
        if vsize_change >= 0 {
            self.fees.insert(bin_index, vsize);
        } else {
            self.fees.remove(bin_index, vsize);
        }
    }
}

pub(crate) struct FeeHistogram {
    /// bins[64-i] contains transactions' statistics inside the fee band of [2**(i-1), 2**i).
    /// bins[64] = [0, 1)
    /// bins[63] = [1, 2)
    /// bins[62] = [2, 4)
    /// bins[61] = [4, 8)
    /// bins[60] = [8, 16)
    /// ...
    /// bins[1] = [2**62, 2**63)
    /// bins[0] = [2**63, 2**64)
    vsize: [u64; FeeHistogram::BINS],
    count: [u64; FeeHistogram::BINS],
}

impl Default for FeeHistogram {
    fn default() -> Self {
        Self {
            vsize: [0; FeeHistogram::BINS],
            count: [0; FeeHistogram::BINS],
        }
    }
}

impl FeeHistogram {
    const BINS: usize = 65; // 0..=64

    fn bin_index(fee: Amount, vsize: u64) -> usize {
        let fee_rate = fee.to_sat() / vsize;
        usize::try_from(fee_rate.leading_zeros()).unwrap()
    }

    fn bin_range(bin_index: usize) -> (u128, u128) {
        let limit = 1u128 << (FeeHistogram::BINS - bin_index - 1);
        (limit / 2, limit)
    }

    fn insert(&mut self, bin_index: usize, vsize: u64) {
        // skip transactions with too low fee rate (<1 sat/vB)
        if let Some(bin) = self.vsize.get_mut(bin_index) {
            *bin += vsize
        }
        if let Some(bin) = self.count.get_mut(bin_index) {
            *bin += 1
        }
    }

    fn remove(&mut self, bin_index: usize, vsize: u64) {
        // skip transactions with too low fee rate (<1 sat/vB)
        if let Some(bin) = self.vsize.get_mut(bin_index) {
            *bin = bin.checked_sub(vsize).unwrap_or_else(|| {
                warn!("removing TX from mempool caused bin count to unexpectedly drop below zero");
                0
            });
        }
        if let Some(bin) = self.count.get_mut(bin_index) {
            *bin = bin.checked_sub(1).unwrap_or_else(|| {
                warn!("removing TX from mempool caused bin vsize to unexpectedly drop below zero");
                0
            });
        }
    }
}

impl Serialize for FeeHistogram {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(self.vsize.len()))?;
        // https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-methods.html#mempool-get-fee-histogram
        let fee_rates =
            (0..FeeHistogram::BINS).map(|i| std::u64::MAX.checked_shr(i as u32).unwrap_or(0));
        fee_rates
            .zip(self.vsize.iter().copied())
            .skip_while(|(_fee_rate, vsize)| *vsize == 0)
            .try_for_each(|element| seq.serialize_element(&element))?;
        seq.end()
    }
}

#[cfg(test)]
mod tests {
    use super::FeeHistogram;
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
            (Amount::from_sat(1), 100),
        ];
        let mut hist = FeeHistogram::default();
        for (amount, vsize) in items {
            let bin_index = FeeHistogram::bin_index(amount, vsize);
            hist.insert(bin_index, vsize);
        }
        assert_eq!(
            json!(hist),
            json!([[15, 10], [7, 40], [3, 20], [1, 10], [0, 100]])
        );

        {
            let bin_index = FeeHistogram::bin_index(Amount::from_sat(5), 1); // 5 sat/byte
            hist.remove(bin_index, 11);
            assert_eq!(
                json!(hist),
                json!([[15, 10], [7, 29], [3, 20], [1, 10], [0, 100]])
            );
        }

        {
            let bin_index = FeeHistogram::bin_index(Amount::from_sat(13), 1); // 13 sat/byte
            hist.insert(bin_index, 80);
            assert_eq!(
                json!(hist),
                json!([[15, 90], [7, 29], [3, 20], [1, 10], [0, 100]])
            );
        }

        {
            let bin_index = FeeHistogram::bin_index(Amount::from_sat(99), 1); // 99 sat/byte
            hist.insert(bin_index, 15);
            assert_eq!(
                json!(hist),
                json!([
                    [127, 15],
                    [63, 0],
                    [31, 0],
                    [15, 90],
                    [7, 29],
                    [3, 20],
                    [1, 10],
                    [0, 100]
                ])
            );
        }
    }
}
