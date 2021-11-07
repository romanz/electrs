use anyhow::Result;
use bitcoin::{
    hashes::{sha256, Hash, HashEngine},
    Amount, Block, BlockHash, OutPoint, SignedAmount, Transaction, Txid,
};
use rayon::prelude::*;
use serde::ser::{Serialize, Serializer};

use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::TryFrom;

use crate::{
    cache::Cache,
    chain::Chain,
    daemon::Daemon,
    index::Index,
    mempool::Mempool,
    merkle::Proof,
    types::{ScriptHash, StatusHash},
};

/// Given a scripthash, store relevant inputs and outputs of a specific transaction
struct TxEntry {
    txid: Txid,
    outputs: Vec<TxOutput>, // relevant funded outputs and their amounts
    spent: Vec<OutPoint>,   // relevant spent outpoints
}

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
// https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-get-history
// https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-get-mempool
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
// https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-get-history
// https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-get-mempool
#[derive(Serialize)]
pub(crate) struct HistoryEntry {
    #[serde(rename = "tx_hash")]
    txid: Txid,
    height: Height,
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "bitcoin::util::amount::serde::as_sat::opt"
    )]
    fee: Option<Amount>,
}

impl HistoryEntry {
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

/// ScriptHash subscription status
pub struct ScriptHashStatus {
    scripthash: ScriptHash, // specfic scripthash to be queried
    tip: BlockHash,         // used for skipping confirmed entries' sync
    confirmed: HashMap<BlockHash, Vec<TxEntry>>, // confirmed entries, partitioned per block (may contain stale blocks)
    mempool: Vec<TxEntry>,                       // unconfirmed entries
    history: Vec<HistoryEntry>,                  // computed from confirmed and mempool entries
    statushash: Option<StatusHash>,              // computed from history
}

/// Specific scripthash balance
#[derive(Default, Eq, PartialEq, Serialize)]
pub(crate) struct Balance {
    #[serde(with = "bitcoin::util::amount::serde::as_sat", rename = "confirmed")]
    confirmed_balance: Amount,
    #[serde(with = "bitcoin::util::amount::serde::as_sat", rename = "unconfirmed")]
    mempool_delta: SignedAmount,
}

// A single unspent transaction output entry:
// https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-listunspent
#[derive(Serialize)]
pub(crate) struct UnspentEntry {
    height: usize, // 0 = mempool entry
    tx_hash: Txid,
    tx_pos: u32,
    #[serde(with = "bitcoin::util::amount::serde::as_sat")]
    value: Amount,
}

#[derive(Default)]
struct Unspent {
    // mapping an outpoint to its value & confirmation height
    outpoints: HashMap<OutPoint, (Amount, usize)>,
    confirmed_balance: Amount,
    mempool_delta: SignedAmount,
}

impl Unspent {
    fn build(status: &ScriptHashStatus, chain: &Chain) -> Self {
        let mut unspent = Unspent::default();

        status
            .confirmed_height_entries(chain)
            .for_each(|(height, entries)| entries.iter().for_each(|e| unspent.insert(e, height)));
        status
            .confirmed_entries(chain)
            .for_each(|e| unspent.remove(e));

        unspent.confirmed_balance = unspent.balance();

        status.mempool.iter().for_each(|e| unspent.insert(e, 0)); // mempool height = 0
        status.mempool.iter().for_each(|e| unspent.remove(e));

        unspent.mempool_delta =
            unspent.balance().to_signed().unwrap() - unspent.confirmed_balance.to_signed().unwrap();

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
            tip: BlockHash::default(),
            confirmed: HashMap::new(),
            mempool: Vec::new(),
            history: Vec::new(),
            statushash: None,
        }
    }

    /// Iterate through confirmed TxEntries with their corresponding block heights.
    /// Skip entries from stale blocks.
    fn confirmed_height_entries<'a>(
        &'a self,
        chain: &'a Chain,
    ) -> impl Iterator<Item = (usize, &[TxEntry])> + 'a {
        self.confirmed
            .iter()
            .filter_map(move |(blockhash, entries)| {
                chain
                    .get_block_height(blockhash)
                    .map(|height| (height, &entries[..]))
            })
    }

    /// Iterate through confirmed TxEntries.
    /// Skip entries from stale blocks.
    fn confirmed_entries<'a>(&'a self, chain: &'a Chain) -> impl Iterator<Item = &TxEntry> + 'a {
        self.confirmed_height_entries(chain)
            .flat_map(|(_height, entries)| entries)
    }

    /// Collect all funded and confirmed outpoints (as a set).
    fn confirmed_outpoints(&self, chain: &Chain) -> HashSet<OutPoint> {
        self.confirmed_entries(chain)
            .flat_map(TxEntry::funding_outpoints)
            .collect()
    }

    pub(crate) fn get_unspent(&self, chain: &Chain) -> Vec<UnspentEntry> {
        Unspent::build(self, chain).into_entries()
    }

    pub(crate) fn get_balance(&self, chain: &Chain) -> Balance {
        let unspent = Unspent::build(self, chain);
        Balance {
            confirmed_balance: unspent.confirmed_balance,
            mempool_delta: unspent.mempool_delta,
        }
    }

    pub(crate) fn get_history(&self) -> &[HistoryEntry] {
        &self.history
    }

    /// Collect all confirmed history entries (in block order).
    fn get_confirmed_history(&self, chain: &Chain) -> Vec<HistoryEntry> {
        self.confirmed_height_entries(chain)
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

    /// Apply func only on the new blocks (fetched from daemon).
    fn for_new_blocks<B, F>(&self, blockhashes: B, daemon: &Daemon, func: F) -> Result<()>
    where
        B: IntoIterator<Item = BlockHash>,
        F: FnMut(BlockHash, Block),
    {
        daemon.for_blocks(
            blockhashes
                .into_iter()
                .filter(|blockhash| !self.confirmed.contains_key(blockhash)),
            func,
        )
    }

    /// Get funding and spending entries from new blocks.
    /// Also cache relevant transactions and their merkle proofs.
    fn sync_confirmed(
        &self,
        index: &Index,
        daemon: &Daemon,
        cache: &Cache,
        cache_proofs: bool,
        outpoints: &mut HashSet<OutPoint>,
    ) -> Result<HashMap<BlockHash, Vec<TxEntry>>> {
        let scripthash = self.scripthash;
        let mut result = HashMap::<BlockHash, HashMap<usize, TxEntry>>::new();

        let funding_blockhashes = index.limit_result(index.filter_by_funding(scripthash))?;
        self.for_new_blocks(funding_blockhashes, daemon, |blockhash, block| {
            let block_entries = result.entry(blockhash).or_default();
            filter_block_txs(block, |tx| filter_outputs(&tx, scripthash), cache_proofs).for_each(
                |FilteredTx {
                     pos,
                     tx,
                     txid,
                     proof,
                     result: funding_outputs,
                 }| {
                    cache.add_tx(txid, move || tx);
                    if let Some(proof) = proof {
                        cache.add_proof(blockhash, txid, proof);
                    }
                    outpoints.extend(make_outpoints(txid, &funding_outputs));
                    block_entries
                        .entry(pos)
                        .or_insert_with(|| TxEntry::new(txid))
                        .outputs = funding_outputs;
                },
            );
        })?;
        let spending_blockhashes: HashSet<BlockHash> = outpoints
            .par_iter()
            .flat_map_iter(|outpoint| index.filter_by_spending(*outpoint))
            .collect();
        self.for_new_blocks(spending_blockhashes, daemon, |blockhash, block| {
            let block_entries = result.entry(blockhash).or_default();
            filter_block_txs(block, |tx| filter_inputs(&tx, outpoints), cache_proofs).for_each(
                |FilteredTx {
                     pos,
                     tx,
                     txid,
                     proof,
                     result: spent_outpoints,
                 }| {
                    cache.add_tx(txid, move || tx);
                    if let Some(proof) = proof {
                        cache.add_proof(blockhash, txid, proof);
                    }
                    block_entries
                        .entry(pos)
                        .or_insert_with(|| TxEntry::new(txid))
                        .spent = spent_outpoints;
                },
            );
        })?;

        Ok(result
            .into_iter()
            .map(|(blockhash, entries_map)| {
                // sort transactions by their position in a block
                let sorted_entries = entries_map
                    .into_iter()
                    .collect::<BTreeMap<usize, TxEntry>>()
                    .into_iter()
                    .map(|(_pos, entry)| entry)
                    .collect::<Vec<TxEntry>>();
                (blockhash, sorted_entries)
            })
            .collect())
    }

    /// Get funding and spending entries from current mempool.
    /// Also cache relevant transactions.
    fn sync_mempool(
        &self,
        mempool: &Mempool,
        cache: &Cache,
        outpoints: &mut HashSet<OutPoint>,
    ) -> Vec<TxEntry> {
        let mut result = HashMap::<Txid, TxEntry>::new();
        for entry in mempool.filter_by_funding(&self.scripthash) {
            let funding_outputs = filter_outputs(&entry.tx, self.scripthash);
            assert!(!funding_outputs.is_empty());
            outpoints.extend(make_outpoints(entry.txid, &funding_outputs));
            result
                .entry(entry.txid)
                .or_insert_with(|| TxEntry::new(entry.txid))
                .outputs = funding_outputs;
            cache.add_tx(entry.txid, || entry.tx.clone());
        }
        for entry in outpoints
            .iter()
            .flat_map(|outpoint| mempool.filter_by_spending(outpoint))
        {
            let spent_outpoints = filter_inputs(&entry.tx, outpoints);
            assert!(!spent_outpoints.is_empty());
            result
                .entry(entry.txid)
                .or_insert_with(|| TxEntry::new(entry.txid))
                .spent = spent_outpoints;
            cache.add_tx(entry.txid, || entry.tx.clone());
        }
        result.into_iter().map(|(_txid, entry)| entry).collect()
    }

    /// Sync with currently confirmed txs and mempool, downloading non-cached transactions via p2p protocol.
    /// After a successful sync, scripthash status is updated.
    pub(crate) fn sync(
        &mut self,
        index: &Index,
        mempool: &Mempool,
        daemon: &Daemon,
        cache: &Cache,
        cache_proofs: bool,
    ) -> Result<()> {
        let mut outpoints: HashSet<OutPoint> = self.confirmed_outpoints(index.chain());

        let new_tip = index.chain().tip();
        if self.tip != new_tip {
            let update = self.sync_confirmed(index, daemon, cache, cache_proofs, &mut outpoints)?;
            self.confirmed.extend(update);
            self.tip = new_tip;
        }
        if !self.confirmed.is_empty() {
            debug!(
                "{} transactions from {} blocks",
                self.confirmed.values().map(Vec::len).sum::<usize>(),
                self.confirmed.len()
            );
        }
        self.mempool = self.sync_mempool(mempool, cache, &mut outpoints);
        if !self.mempool.is_empty() {
            debug!("{} mempool transactions", self.mempool.len());
        }
        self.history.clear();
        self.history
            .extend(self.get_confirmed_history(index.chain()));
        self.history.extend(self.get_mempool_history(mempool));

        self.statushash = compute_status_hash(&self.history);
        Ok(())
    }

    /// Get current status hash.
    pub fn statushash(&self) -> Option<StatusHash> {
        self.statushash
    }
}

fn make_outpoints<'a>(txid: Txid, outputs: &'a [TxOutput]) -> impl Iterator<Item = OutPoint> + 'a {
    outputs
        .iter()
        .map(move |out| OutPoint::new(txid, out.index))
}

fn filter_outputs(tx: &Transaction, scripthash: ScriptHash) -> Vec<TxOutput> {
    let outputs = tx.output.iter().zip(0u32..);
    outputs
        .filter_map(move |(txo, vout)| {
            if ScriptHash::new(&txo.script_pubkey) == scripthash {
                Some(TxOutput {
                    index: vout,
                    value: Amount::from_sat(txo.value),
                })
            } else {
                None
            }
        })
        .collect()
}

fn filter_inputs(tx: &Transaction, outpoints: &HashSet<OutPoint>) -> Vec<OutPoint> {
    tx.input
        .iter()
        .filter_map(|txi| {
            if outpoints.contains(&txi.previous_output) {
                Some(txi.previous_output)
            } else {
                None
            }
        })
        .collect()
}

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

struct FilteredTx<T> {
    tx: Transaction,
    txid: Txid,
    pos: usize,
    proof: Option<Proof>,
    result: Vec<T>,
}

fn filter_block_txs<T: Send>(
    block: Block,
    map_fn: impl Fn(&Transaction) -> Vec<T> + Sync,
    cache_proofs: bool,
) -> impl Iterator<Item = FilteredTx<T>> {
    let txids: Vec<Txid> = if cache_proofs {
        block.txdata.par_iter().map(|tx| tx.txid()).collect()
    } else {
        vec![] // txids are not needed if we don't cache merkle proofs
    };
    block
        .txdata
        .into_par_iter()
        .enumerate()
        .filter_map(|(pos, tx)| {
            let result = map_fn(&tx);
            if result.is_empty() {
                return None; // skip irrelevant transaction
            }
            let txid = txids.get(pos).copied().unwrap_or_else(|| tx.txid());
            Some(FilteredTx {
                tx,
                txid,
                pos,
                proof: if cache_proofs {
                    Some(Proof::create(&txids, pos))
                } else {
                    None
                },
                result,
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
}

#[cfg(test)]
mod tests {
    use super::HistoryEntry;
    use bitcoin::{hashes::hex::FromHex, Amount, Txid};
    use serde_json::json;

    #[test]
    fn test_txinfo_json() {
        let txid =
            Txid::from_hex("5b75086dafeede555fc8f9a810d8b10df57c46f9f176ccc3dd8d2fa20edd685b")
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
