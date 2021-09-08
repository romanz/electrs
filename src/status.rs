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
    outputs: Vec<u32>,    // relevant funded output indices
    spent: Vec<OutPoint>, // relevant spent outpoints
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
    fn funding(&self) -> impl Iterator<Item = OutPoint> + '_ {
        make_outpoints(&self.txid, &self.outputs)
    }

    /// Relevant (scripthash-wise) spent outpoints
    fn spending(&self) -> impl Iterator<Item = OutPoint> + '_ {
        self.spent.iter().copied()
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

enum BalanceEntry {
    Funded(OutPoint),
    Spent(OutPoint),
}

/// Specific scripthash balance
#[derive(Default, Eq, PartialEq)]
pub(crate) struct Balance {
    pub(crate) confirmed: Amount,
    pub(crate) mempool_delta: SignedAmount,
}

#[derive(Default)]
struct Total {
    funded: Amount,
    spent: Amount,
}

impl Total {
    fn balance(&self) -> Amount {
        self.funded - self.spent
    }

    fn update(
        &mut self,
        entries: impl Iterator<Item = BalanceEntry>,
        get_amount: impl Fn(OutPoint) -> Amount,
    ) {
        for entry in entries {
            match entry {
                BalanceEntry::Funded(outpoint) => self.funded += get_amount(outpoint),
                BalanceEntry::Spent(outpoint) => self.spent += get_amount(outpoint),
            }
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
    fn confirmed_entries<'a>(
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

    /// Collect all funded and confirmed outpoints (as a set).
    fn confirmed_outpoints(&self, chain: &Chain) -> HashSet<OutPoint> {
        self.confirmed_entries(chain)
            .flat_map(|(_height, entries)| entries.iter().flat_map(TxEntry::funding))
            .collect()
    }

    pub(crate) fn get_balance<F>(&self, chain: &Chain, get_amount: F) -> Balance
    where
        F: Fn(OutPoint) -> Amount,
    {
        fn to_balance_entries<'a>(
            entries: impl Iterator<Item = &'a TxEntry> + 'a,
        ) -> impl Iterator<Item = BalanceEntry> + 'a {
            entries.flat_map(|e| {
                let funded = e.funding().map(BalanceEntry::Funded);
                let spent = e.spending().map(BalanceEntry::Spent);
                funded.chain(spent)
            })
        }

        let confirmed_entries = to_balance_entries(
            self.confirmed_entries(chain)
                .flat_map(|(_height, entries)| entries),
        );
        let mempool_entries = to_balance_entries(self.mempool.iter());

        let mut total = Total::default();
        total.update(confirmed_entries, &get_amount);
        let confirmed = total.balance();

        total.update(mempool_entries, &get_amount);
        let with_mempool = total.balance();

        Balance {
            confirmed,
            mempool_delta: with_mempool.to_signed().unwrap() - confirmed.to_signed().unwrap(),
        }
    }

    pub(crate) fn get_history(&self, chain: &Chain, mempool: &Mempool) -> Vec<HistoryEntry> {
        let mut result = self.get_confirmed_history(chain);
        result.extend(self.get_mempool_history(mempool));
        result
    }

    /// Collect all confirmed history entries (in block order).
    fn get_confirmed_history(&self, chain: &Chain) -> Vec<HistoryEntry> {
        self.confirmed_entries(chain)
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
        F: FnMut(BlockHash, Block) + Send,
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
        type TxPosition = usize; // transaction position within a block
        let mut result = HashMap::<BlockHash, HashMap<TxPosition, TxEntry>>::new();

        let funding_blockhashes = index.limit_result(index.filter_by_funding(self.scripthash))?;
        self.for_new_blocks(funding_blockhashes, daemon, |blockhash, block| {
            let txids: Vec<Txid> = block.txdata.iter().map(|tx| tx.txid()).collect();
            for (pos, (tx, txid)) in block.txdata.into_iter().zip(txids.iter()).enumerate() {
                let funding_outputs = filter_outputs(&tx, &self.scripthash);
                if funding_outputs.is_empty() {
                    continue;
                }
                cache.add_tx(*txid, move || tx);
                cache.add_proof(blockhash, *txid, || Proof::create(&txids, pos));
                outpoints.extend(make_outpoints(txid, &funding_outputs));
                result
                    .entry(blockhash)
                    .or_default()
                    .entry(pos)
                    .or_insert_with(|| TxEntry::new(*txid))
                    .outputs = funding_outputs;
            }
        })?;
        let spending_blockhashes: HashSet<BlockHash> = outpoints
            .par_iter()
            .flat_map_iter(|outpoint| index.filter_by_spending(*outpoint))
            .collect();
        self.for_new_blocks(spending_blockhashes, daemon, |blockhash, block| {
            let txids: Vec<Txid> = block.txdata.iter().map(|tx| tx.txid()).collect();
            for (pos, (tx, txid)) in block.txdata.into_iter().zip(txids.iter()).enumerate() {
                let spent_outpoints = filter_inputs(&tx, outpoints);
                if spent_outpoints.is_empty() {
                    continue;
                }
                cache.add_tx(*txid, move || tx);
                cache.add_proof(blockhash, *txid, || Proof::create(&txids, pos));
                result
                    .entry(blockhash)
                    .or_default()
                    .entry(pos)
                    .or_insert_with(|| TxEntry::new(*txid))
                    .spent = spent_outpoints;
            }
        })?;

        Ok(result
            .into_iter()
            .map(|(blockhash, entries_map)| {
                // sort transactions by their position in a block
                let sorted_entries = entries_map
                    .into_iter()
                    .collect::<BTreeMap<TxPosition, TxEntry>>()
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
            let funding_outputs = filter_outputs(&entry.tx, &self.scripthash);
            assert!(!funding_outputs.is_empty());
            outpoints.extend(make_outpoints(&entry.txid, &funding_outputs));
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
        self.history = self.get_history(index.chain(), mempool);
        self.statushash = compute_status_hash(&self.history);
        Ok(())
    }

    /// Get current status hash.
    pub fn statushash(&self) -> Option<StatusHash> {
        self.statushash
    }
}

fn make_outpoints<'a>(txid: &'a Txid, outputs: &'a [u32]) -> impl Iterator<Item = OutPoint> + 'a {
    outputs.iter().map(move |vout| OutPoint::new(*txid, *vout))
}

fn filter_outputs(tx: &Transaction, scripthash: &ScriptHash) -> Vec<u32> {
    let outputs = tx.output.iter().zip(0u32..);
    outputs
        .filter_map(move |(txo, vout)| {
            if ScriptHash::new(&txo.script_pubkey) == *scripthash {
                Some(vout)
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
