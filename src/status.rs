use crate::bitcoin::{
    consensus::Decodable,
    hashes::{sha256, Hash, HashEngine},
    Amount, BlockHash, OutPoint, SignedAmount, Transaction, Txid,
};
use anyhow::Result;
use bindex::{IndexedChain, ScriptHash};
use serde::ser::{Serialize, Serializer};

use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::TryFrom;

use crate::{mempool::Mempool, types::StatusHash};

/// Given a scripthash, store relevant inputs and outputs of a specific transaction
struct TxEntry {
    txid: Txid,
    outputs: Vec<TxOutput>, // relevant funded outputs and their amounts
    spent: Vec<OutPoint>,   // relevant spent outpoints
}

// Funded outputs of a transaction
struct TxOutput {
    index: u32,
    value: Amount,
}

impl TxEntry {
    fn new(txid: Txid) -> Self {
        Self {
            txid,
            outputs: Vec::new(),
            spent: Vec::new(),
        }
    }

    /// Relevant (scripthash-wise) funded outpoints
    fn funding_outpoints(&self) -> impl Iterator<Item = OutPoint> + '_ {
        make_outpoints(self.txid, &self.outputs)
    }
}

// Confirmation height of a transaction or its mempool state:
// https://electrum-protocol.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-get-history
// https://electrum-protocol.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-get-mempool
enum Height {
    Confirmed { height: usize },
    Unconfirmed { has_unconfirmed_inputs: bool },
}

impl Height {
    fn as_i64(&self) -> i64 {
        match self {
            Self::Confirmed { height } => i64::try_from(*height).unwrap(),
            Self::Unconfirmed {
                has_unconfirmed_inputs: true,
            } => -1,
            Self::Unconfirmed {
                has_unconfirmed_inputs: false,
            } => 0,
        }
    }
}

impl Serialize for Height {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_i64(self.as_i64())
    }
}

impl std::fmt::Display for Height {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_i64().fmt(f)
    }
}

// A single history entry:
// https://electrum-protocol.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-get-history
// https://electrum-protocol.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-get-mempool
#[derive(Serialize)]
pub(crate) struct HistoryEntry {
    #[serde(rename = "tx_hash")]
    txid: Txid,
    height: Height,
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "crate::bitcoin::amount::serde::as_sat::opt"
    )]
    fee: Option<Amount>,
}

impl HistoryEntry {
    // Hash to compute ScriptHash status, as defined here:
    // https://electrum-protocol.readthedocs.io/en/latest/protocol-basics.html#status
    fn hash(&self, engine: &mut sha256::HashEngine) {
        let s = format!("{}:{}:", self.txid, self.height);
        engine.input(s.as_bytes());
    }

    fn confirmed(txid: Txid, height: usize) -> Self {
        Self {
            txid,
            height: Height::Confirmed { height },
            fee: None,
        }
    }

    fn unconfirmed(txid: Txid, has_unconfirmed_inputs: bool, fee: Amount) -> Self {
        Self {
            txid,
            height: Height::Unconfirmed {
                has_unconfirmed_inputs,
            },
            fee: Some(fee),
        }
    }
}

/// Make sure blocks are sorted by ascending height.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct BlockKey(usize, BlockHash);

impl From<bindex::Location<'_>> for BlockKey {
    fn from(location: bindex::Location<'_>) -> Self {
        Self(location.block_height(), location.block_hash())
    }
}

/// ScriptHash subscription status
pub struct ScriptHashStatus {
    scripthash: ScriptHash, // specific scripthash to be queried
    confirmed: BTreeMap<BlockKey, Vec<TxEntry>>, // confirmed entries (chronologically ordered), partitioned per block
    mempool: Vec<TxEntry>,                       // unconfirmed entries
    history: Vec<HistoryEntry>,                  // computed from confirmed and mempool entries
    statushash: Option<StatusHash>,              // computed from history
}

/// Specific scripthash balance
/// https://electrum-protocol.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-get-balance
#[derive(Default, Eq, PartialEq, Serialize)]
pub(crate) struct Balance {
    #[serde(with = "crate::bitcoin::amount::serde::as_sat", rename = "confirmed")]
    confirmed_balance: Amount,
    #[serde(with = "crate::bitcoin::amount::serde::as_sat", rename = "unconfirmed")]
    mempool_delta: SignedAmount,
}

/// A single unspent transaction output entry
/// https://electrum-protocol.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-listunspent
#[derive(Serialize)]
pub(crate) struct UnspentEntry {
    height: usize, // 0 = mempool entry
    tx_hash: Txid,
    tx_pos: u32,
    #[serde(with = "crate::bitcoin::amount::serde::as_sat")]
    value: Amount,
}

#[derive(Default)]
struct Unspent {
    // mapping an outpoint to its value & confirmation height
    outpoints: HashMap<OutPoint, (Amount, usize)>,
    balance: Balance,
}

impl Unspent {
    fn build(status: &ScriptHashStatus) -> Self {
        let mut unspent = Unspent::default();
        // First, add all relevant entries' funding outputs to the outpoints' map
        status
            .confirmed_height_entries()
            .for_each(|(height, entries)| entries.iter().for_each(|e| unspent.insert(e, height)));
        // Then, remove spent outpoints from the map
        status.confirmed_entries().for_each(|e| unspent.remove(e));

        unspent.balance.confirmed_balance = unspent.balance();
        // Now, do the same over the mempool (first add funding outputs, and then remove spent ones)
        status.mempool.iter().for_each(|e| unspent.insert(e, 0)); // mempool height = 0
        status.mempool.iter().for_each(|e| unspent.remove(e));

        let total_balance = unspent.balance().to_signed().unwrap();
        let confirmed_balance = unspent.balance.confirmed_balance.to_signed().unwrap();
        unspent.balance.mempool_delta = total_balance - confirmed_balance;
        unspent
    }

    fn into_entries(self) -> Vec<UnspentEntry> {
        self.outpoints
            .into_iter()
            .map(|(outpoint, (value, height))| UnspentEntry {
                height,
                tx_hash: outpoint.txid,
                tx_pos: outpoint.vout,
                value,
            })
            .collect()
    }

    /// Total amount of unspent outputs
    fn balance(&self) -> Amount {
        self.outpoints
            .values()
            .fold(Amount::default(), |acc, v| acc + v.0)
    }

    fn insert(&mut self, entry: &TxEntry, height: usize) {
        for output in &entry.outputs {
            let outpoint = OutPoint {
                txid: entry.txid,
                vout: output.index,
            };
            self.outpoints.insert(outpoint, (output.value, height));
        }
    }

    fn remove(&mut self, entry: &TxEntry) {
        for spent in &entry.spent {
            self.outpoints.remove(spent);
        }
    }
}

impl ScriptHashStatus {
    /// Return non-synced (empty) status for a given script hash.
    pub fn new(scripthash: ScriptHash) -> Self {
        Self {
            scripthash,
            confirmed: BTreeMap::new(),
            mempool: Vec::new(),
            history: Vec::new(),
            statushash: None,
        }
    }

    /// Iterate through confirmed TxEntries with their corresponding block heights.
    /// Skip entries from stale blocks.
    fn confirmed_height_entries<'a>(&'a self) -> impl Iterator<Item = (usize, &'a [TxEntry])> + 'a {
        self.confirmed
            .iter()
            .map(move |(blockid, entries)| (blockid.0, &entries[..]))
    }

    /// Iterate through confirmed TxEntries.
    fn confirmed_entries<'a>(&'a self) -> impl Iterator<Item = &'a TxEntry> + 'a {
        self.confirmed_height_entries()
            .flat_map(|(_height, entries)| entries)
    }

    /// Collect all funded and confirmed outpoints (as a set).
    fn confirmed_outpoints(&self) -> HashSet<OutPoint> {
        self.confirmed_entries()
            .flat_map(TxEntry::funding_outpoints)
            .collect()
    }

    /// Collect unspent transaction entries
    pub(crate) fn get_unspent(&self) -> Vec<UnspentEntry> {
        Unspent::build(self).into_entries()
    }

    /// Collect unspent transaction balance
    pub(crate) fn get_balance(&self) -> Balance {
        Unspent::build(self).balance
    }

    /// Collect transaction history entries
    pub(crate) fn get_history(&self) -> &[HistoryEntry] {
        &self.history
    }

    /// Collect all confirmed history entries (in block order).
    fn get_confirmed_history(&self) -> Vec<HistoryEntry> {
        self.confirmed_height_entries()
            .collect::<BTreeMap<usize, &[TxEntry]>>()
            .into_iter()
            .flat_map(|(height, entries)| {
                entries
                    .iter()
                    .map(move |e| HistoryEntry::confirmed(e.txid, height))
            })
            .collect()
    }

    /// Collect all mempool history entries (keeping transactions with unconfirmed parents last).
    fn get_mempool_history(&self, mempool: &Mempool) -> Vec<HistoryEntry> {
        let mut entries = self
            .mempool
            .iter()
            .filter_map(|e| mempool.get(&e.txid))
            .collect::<Vec<_>>();
        entries.sort_by_key(|e| (e.has_unconfirmed_inputs, e.txid));
        entries
            .into_iter()
            .map(|e| HistoryEntry::unconfirmed(e.txid, e.has_unconfirmed_inputs, e.fee))
            .collect()
    }

    /// Get funding and spending entries from new blocks.
    /// Also cache relevant transactions and their merkle proofs.
    fn sync_confirmed(&mut self, index: &IndexedChain) -> Result<HashSet<OutPoint>> {
        let headers = index.headers();
        let mut latest_header = None;
        // Drop entries from stale blocks
        while let Some(entry) = self.confirmed.last_entry() {
            let BlockKey(height, hash) = entry.key();
            match headers.get_header(*hash, *height) {
                Ok(header) => {
                    latest_header = Some(header);
                    break;
                }
                Err(err) => {
                    warn!("drop reorged block: {}", err);
                    entry.remove();
                    continue;
                }
            }
        }
        // Recompute all funded outpoints
        let mut outpoints = self.confirmed_outpoints();
        // Process transactions in chronological order
        for location in index.locations_by_scripthash(&self.scripthash, latest_header)? {
            let tx_bytes = index.get_tx_bytes(&location)?;
            let tx = Transaction::consensus_decode_from_finite_reader(&mut &tx_bytes[..])?;

            // Check if this transaction has relevant inputs/outputs:
            let spent = filter_inputs(&tx, &outpoints);
            let outputs = filter_outputs(&tx, self.scripthash);
            if spent.is_empty() && outputs.is_empty() {
                continue;
            }

            // Build new TxEntry and add new outpoints (for next transactions)
            let mut tx_entry = TxEntry::new(tx.compute_txid());
            tx_entry.spent = spent;
            tx_entry.outputs = outputs;
            outpoints.extend(tx_entry.funding_outpoints());

            // Add new per-block entry (if needed)
            self.confirmed
                .entry(BlockKey::from(location))
                .or_default()
                .push(tx_entry);
        }

        Ok(outpoints)
    }

    /// Get funding and spending entries from current mempool.
    /// Also cache relevant transactions.
    fn sync_mempool(&self, mempool: &Mempool, mut outpoints: HashSet<OutPoint>) -> Vec<TxEntry> {
        let mut result = HashMap::<Txid, TxEntry>::new();
        // extract relevant funding transactions
        for entry in mempool.filter_by_funding(&self.scripthash) {
            let funding_outputs = filter_outputs(&entry.tx, self.scripthash);
            assert!(!funding_outputs.is_empty());
            // store funded outpoints (to check for spending later)
            outpoints.extend(make_outpoints(entry.txid, &funding_outputs));
            result
                .entry(entry.txid) // the transaction may already exist
                .or_insert_with(|| TxEntry::new(entry.txid))
                .outputs = funding_outputs;
        }
        for entry in outpoints
            .iter()
            .flat_map(|outpoint| mempool.filter_by_spending(outpoint))
        {
            let spent_outpoints = filter_inputs(&entry.tx, &outpoints);
            assert!(!spent_outpoints.is_empty());
            result
                .entry(entry.txid) // the transaction may already exist
                .or_insert_with(|| TxEntry::new(entry.txid))
                .spent = spent_outpoints;
        }
        result.into_values().collect()
    }

    /// Sync with currently confirmed txs and mempool, downloading non-cached transactions via REST API.
    /// After a successful sync, scripthash status is updated.
    pub(crate) fn sync(&mut self, index: &IndexedChain, mempool: &Mempool) -> Result<()> {
        let outpoints = self.sync_confirmed(index)?;
        if !self.confirmed.is_empty() {
            debug!(
                "{} transactions from {} blocks",
                self.confirmed.values().map(Vec::len).sum::<usize>(),
                self.confirmed.len()
            );
        }
        self.mempool = self.sync_mempool(mempool, outpoints);
        if !self.mempool.is_empty() {
            debug!("{} mempool transactions", self.mempool.len());
        }
        // update history entries and status hash
        self.history.clear();
        self.history.extend(self.get_confirmed_history());
        self.history.extend(self.get_mempool_history(mempool));

        self.statushash = compute_status_hash(&self.history);
        Ok(())
    }

    /// Get current status hash.
    pub fn statushash(&self) -> Option<StatusHash> {
        self.statushash
    }
}

fn make_outpoints(txid: Txid, outputs: &[TxOutput]) -> impl Iterator<Item = OutPoint> + '_ {
    outputs
        .iter()
        .map(move |out| OutPoint::new(txid, out.index))
}

/// Collect outputs that fund given scripthash.
fn filter_outputs(tx: &Transaction, scripthash: ScriptHash) -> Vec<TxOutput> {
    let outputs = tx.output.iter().zip(0u32..);
    outputs
        .filter(|&(txo, _vout)| ScriptHash::new(&txo.script_pubkey) == scripthash)
        .map(|(txo, vout)| TxOutput {
            index: vout,
            value: txo.value,
        })
        .collect()
}

/// Collect inputs that spend one of the specified outpoints.
fn filter_inputs(tx: &Transaction, outpoints: &HashSet<OutPoint>) -> Vec<OutPoint> {
    let inputs = tx.input.iter();
    inputs
        .filter(|&txi| outpoints.contains(&txi.previous_output))
        .map(|txi| txi.previous_output)
        .collect()
}

// See https://electrum-protocol.readthedocs.io/en/latest/protocol-basics.html#status for details
fn compute_status_hash(history: &[HistoryEntry]) -> Option<StatusHash> {
    if history.is_empty() {
        return None;
    }
    let mut engine = StatusHash::engine();
    for entry in history {
        entry.hash(&mut engine);
    }
    Some(StatusHash::from_engine(engine))
}

#[cfg(test)]
mod tests {
    use super::HistoryEntry;
    use crate::bitcoin::Amount;
    use serde_json::json;

    #[test]
    fn test_txinfo_json() {
        let txid = "5b75086dafeede555fc8f9a810d8b10df57c46f9f176ccc3dd8d2fa20edd685b"
            .parse()
            .unwrap();
        assert_eq!(
            json!(HistoryEntry::confirmed(txid, 123456)),
            json!({"tx_hash": "5b75086dafeede555fc8f9a810d8b10df57c46f9f176ccc3dd8d2fa20edd685b", "height": 123456})
        );
        assert_eq!(
            json!(HistoryEntry::unconfirmed(txid, true, Amount::from_sat(123))),
            json!({"tx_hash": "5b75086dafeede555fc8f9a810d8b10df57c46f9f176ccc3dd8d2fa20edd685b", "height": -1, "fee": 123})
        );
        assert_eq!(
            json!(HistoryEntry::unconfirmed(
                txid,
                false,
                Amount::from_sat(123)
            )),
            json!({"tx_hash": "5b75086dafeede555fc8f9a810d8b10df57c46f9f176ccc3dd8d2fa20edd685b", "height": 0, "fee": 123})
        );
    }
}
