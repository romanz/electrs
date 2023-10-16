use anyhow::Result;
use bitcoin::{
    consensus::Decodable,
    hashes::{sha256, Hash, HashEngine},
    Amount, BlockHash, OutPoint, SignedAmount, Transaction, Txid,
};
use bitcoin_slices::{bsl, Visit, Visitor};
use rayon::prelude::*;
use serde::ser::{Serialize, Serializer};

use std::convert::TryFrom;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ops::ControlFlow,
};

use crate::{
    cache::Cache,
    chain::Chain,
    daemon::Daemon,
    index::Index,
    mempool::Mempool,
    types::{bsl_txid, ScriptHash, SerBlock, StatusHash},
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
        with = "bitcoin::amount::serde::as_sat::opt"
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
    scripthash: ScriptHash, // specific scripthash to be queried
    tip: BlockHash,         // used for skipping confirmed entries' sync
    confirmed: HashMap<BlockHash, Vec<TxEntry>>, // confirmed entries, partitioned per block (may contain stale blocks)
    mempool: Vec<TxEntry>,                       // unconfirmed entries
    history: Vec<HistoryEntry>,                  // computed from confirmed and mempool entries
    statushash: Option<StatusHash>,              // computed from history
}

/// Specific scripthash balance
#[derive(Default, Eq, PartialEq, Serialize)]
pub(crate) struct Balance {
    #[serde(with = "bitcoin::amount::serde::as_sat", rename = "confirmed")]
    confirmed_balance: Amount,
    #[serde(with = "bitcoin::amount::serde::as_sat", rename = "unconfirmed")]
    mempool_delta: SignedAmount,
}

// A single unspent transaction output entry:
// https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-listunspent
#[derive(Serialize)]
pub(crate) struct UnspentEntry {
    height: usize, // 0 = mempool entry
    tx_hash: Txid,
    tx_pos: u32,
    #[serde(with = "bitcoin::amount::serde::as_sat")]
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
            tip: BlockHash::all_zeros(),
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
        F: FnMut(BlockHash, SerBlock),
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
        outpoints: &mut HashSet<OutPoint>,
    ) -> Result<HashMap<BlockHash, Vec<TxEntry>>> {
        let scripthash = self.scripthash;
        let mut result = HashMap::<BlockHash, HashMap<usize, TxEntry>>::new();

        let funding_blockhashes = index.limit_result(index.filter_by_funding(scripthash))?;
        self.for_new_blocks(funding_blockhashes, daemon, |blockhash, block| {
            let block_entries = result.entry(blockhash).or_default();
            for filtered_outputs in filter_block_txs_outputs(block, scripthash) {
                cache.add_tx(filtered_outputs.txid, move || filtered_outputs.tx);
                outpoints.extend(make_outpoints(
                    filtered_outputs.txid,
                    &filtered_outputs.result,
                ));
                block_entries
                    .entry(filtered_outputs.pos)
                    .or_insert_with(|| TxEntry::new(filtered_outputs.txid))
                    .outputs = filtered_outputs.result;
            }
        })?;
        let spending_blockhashes: HashSet<BlockHash> = outpoints
            .par_iter()
            .flat_map_iter(|outpoint| index.filter_by_spending(*outpoint))
            .collect();
        self.for_new_blocks(spending_blockhashes, daemon, |blockhash, block| {
            let block_entries = result.entry(blockhash).or_default();
            for filtered_inputs in filter_block_txs_inputs(&block, outpoints) {
                cache.add_tx(filtered_inputs.txid, move || filtered_inputs.tx);
                block_entries
                    .entry(filtered_inputs.pos)
                    .or_insert_with(|| TxEntry::new(filtered_inputs.txid))
                    .spent = filtered_inputs.result;
            }
        })?;

        Ok(result
            .into_iter()
            .map(|(blockhash, entries_map)| {
                // sort transactions by their position in a block
                let sorted_entries = entries_map
                    .into_iter()
                    .collect::<BTreeMap<usize, TxEntry>>()
                    .into_values()
                    .collect();
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
        result.into_values().collect()
    }

    /// Sync with currently confirmed txs and mempool, downloading non-cached transactions via p2p protocol.
    /// After a successful sync, scripthash status is updated.
    pub(crate) fn sync(
        &mut self,
        index: &Index,
        mempool: &Mempool,
        daemon: &Daemon,
        cache: &Cache,
    ) -> Result<()> {
        let mut outpoints: HashSet<OutPoint> = self.confirmed_outpoints(index.chain());

        let new_tip = index.chain().tip();
        if self.tip != new_tip {
            let update = self.sync_confirmed(index, daemon, cache, &mut outpoints)?;
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

fn make_outpoints(txid: Txid, outputs: &[TxOutput]) -> impl Iterator<Item = OutPoint> + '_ {
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
                    value: txo.value,
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
    result: Vec<T>,
}

fn filter_block_txs_outputs(block: SerBlock, scripthash: ScriptHash) -> Vec<FilteredTx<TxOutput>> {
    struct FindOutputs {
        scripthash: ScriptHash,
        result: Vec<FilteredTx<TxOutput>>,
        buffer: Vec<TxOutput>,
        pos: usize,
    }
    impl Visitor for FindOutputs {
        fn visit_transaction(&mut self, tx: &bsl::Transaction) -> ControlFlow<()> {
            if !self.buffer.is_empty() {
                let result = std::mem::take(&mut self.buffer);
                let txid = bsl_txid(tx);
                let tx = bitcoin::Transaction::consensus_decode(&mut tx.as_ref())
                    .expect("transaction was already validated");
                self.result.push(FilteredTx::<TxOutput> {
                    tx,
                    txid,
                    pos: self.pos,
                    result,
                });
            }
            self.pos += 1;
            ControlFlow::Continue(())
        }
        fn visit_tx_out(&mut self, vout: usize, tx_out: &bsl::TxOut) -> ControlFlow<()> {
            let current = ScriptHash::hash(tx_out.script_pubkey());
            if current == self.scripthash {
                self.buffer.push(TxOutput {
                    index: vout as u32,
                    value: Amount::from_sat(tx_out.value()),
                })
            }
            ControlFlow::Continue(())
        }
    }
    let mut find_outputs = FindOutputs {
        scripthash,
        result: vec![],
        buffer: vec![],
        pos: 0,
    };

    bsl::Block::visit(&block, &mut find_outputs).expect("core returned invalid block");

    find_outputs.result
}

fn filter_block_txs_inputs(
    block: &SerBlock,
    outpoints: &HashSet<OutPoint>,
) -> Vec<FilteredTx<OutPoint>> {
    struct FindInputs<'a> {
        outpoints: &'a HashSet<OutPoint>,
        result: Vec<FilteredTx<OutPoint>>,
        buffer: Vec<OutPoint>,
        pos: usize,
    }

    impl<'a> Visitor for FindInputs<'a> {
        fn visit_transaction(&mut self, tx: &bsl::Transaction) -> ControlFlow<()> {
            if !self.buffer.is_empty() {
                let result = std::mem::take(&mut self.buffer);
                let txid = bsl_txid(tx);
                let tx = bitcoin::Transaction::consensus_decode(&mut tx.as_ref())
                    .expect("transaction was already validated");
                self.result.push(FilteredTx::<OutPoint> {
                    tx,
                    txid,
                    pos: self.pos,
                    result,
                });
            }
            self.pos += 1;
            ControlFlow::Continue(())
        }
        fn visit_tx_in(&mut self, _vin: usize, tx_in: &bsl::TxIn) -> ControlFlow<()> {
            let current: OutPoint = tx_in.prevout().into();
            if self.outpoints.contains(&current) {
                self.buffer.push(current);
            }
            ControlFlow::Continue(())
        }
    }

    let mut find_inputs = FindInputs {
        outpoints,
        result: vec![],
        buffer: vec![],
        pos: 0,
    };

    bsl::Block::visit(block, &mut find_inputs).expect("core returned invalid block");

    find_inputs.result
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, str::FromStr};

    use crate::types::ScriptHash;

    use super::HistoryEntry;
    use bitcoin::{Address, Amount};
    use bitcoin_test_data::blocks::mainnet_702861;
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

    #[test]
    fn test_find_outputs() {
        let block = mainnet_702861().to_vec();

        let addr = Address::from_str("1A9MXXG26vZVySrNNytQK1N8bX42ZuJ6Ax")
            .unwrap()
            .assume_checked();
        let scripthash = ScriptHash::new(&addr.script_pubkey());

        let result = &super::filter_block_txs_outputs(block, scripthash)[0];
        assert_eq!(
            result.txid.to_string(),
            "7bcdcb44422da5a99daad47d6ba1c3d6f2e48f961a75e42c4fa75029d4b0ef49"
        );
        assert_eq!(result.pos, 8);
        assert_eq!(result.result[0].index, 0);
        assert_eq!(result.result[0].value.to_sat(), 709503);
    }

    #[test]
    fn test_find_inputs() {
        let block = mainnet_702861().to_vec();
        let outpoint = bitcoin::OutPoint::from_str(
            "cc135e792b37a9c4ffd784f696b1e38bd1197f8e67ae1f96c9f13e4618b91866:3",
        )
        .unwrap();
        let mut outpoints = HashSet::new();
        outpoints.insert(outpoint);

        let result = &super::filter_block_txs_inputs(&block, &outpoints)[0];
        assert_eq!(
            result.txid.to_string(),
            "7bcdcb44422da5a99daad47d6ba1c3d6f2e48f961a75e42c4fa75029d4b0ef49"
        );
        assert_eq!(result.pos, 8);
        assert_eq!(result.result[0], outpoint);
    }
}
