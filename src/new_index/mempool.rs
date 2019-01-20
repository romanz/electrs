use bitcoin::consensus::encode::serialize;
use bitcoin::util::hash::Sha256dHash;
use itertools::Itertools;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::iter::FromIterator;
use std::sync::Arc;

use crate::chain::{OutPoint, Transaction, TxOut};
use crate::daemon::Daemon;
use crate::new_index::{
    compute_script_hash, parse_hash, schema::FullHash, ChainQuery, ScriptStats, SpendingInput,
    TxHistoryInfo, Utxo,
};
use crate::util::Bytes;

use crate::errors::*;

pub struct Mempool {
    chain: Arc<ChainQuery>,
    txstore: HashMap<Sha256dHash, Transaction>,
    history: HashMap<FullHash, Vec<TxHistoryInfo>>, // ScriptHash -> {history_entries}
    edges: HashMap<OutPoint, (Sha256dHash, u32)>,   // OutPoint -> (spending_txid, spending_vin)
}

impl Mempool {
    pub fn new(chain: Arc<ChainQuery>) -> Self {
        Mempool {
            chain,
            txstore: HashMap::new(),
            history: HashMap::new(),
            edges: HashMap::new(),
        }
    }

    pub fn lookup_txn(&self, txid: &Sha256dHash) -> Option<Transaction> {
        self.txstore.get(txid).map(|item| item.clone())
    }

    pub fn lookup_raw_txn(&self, txid: &Sha256dHash) -> Option<Bytes> {
        self.txstore.get(txid).map(serialize)
    }

    pub fn lookup_spend(&self, outpoint: &OutPoint) -> Option<SpendingInput> {
        self.edges.get(outpoint).map(|(txid, vin)| SpendingInput {
            txid: *txid,
            vin: *vin,
            confirmed: None,
        })
    }

    pub fn history(&self, scripthash: &[u8]) -> Vec<Transaction> {
        match self.history.get(scripthash) {
            None => return vec![],
            Some(entries) => entries
                .iter()
                .map(get_entry_txid)
                .unique()
                .map(|txid| self.txstore.get(&txid).expect("missing mempool tx"))
                .cloned()
                .collect(),
        }
    }

    pub fn utxo(&self, scripthash: &[u8]) -> Vec<Utxo> {
        let entries = match self.history.get(scripthash) {
            None => return vec![],
            Some(entries) => entries,
        };

        entries
            .into_iter()
            .filter_map(|entry| match entry {
                TxHistoryInfo::Funding(txid, vout, value) => Some(Utxo {
                    txid: parse_hash(txid),
                    vout: *vout as u32,
                    value: *value,
                    confirmed: None,
                }),
                TxHistoryInfo::Spending(..) => None,
            })
            .filter(|utxo| {
                self.edges
                    .get(&OutPoint {
                        txid: utxo.txid,
                        vout: utxo.vout,
                    })
                    .is_none()
            })
            .collect()
    }

    // @XXX avoid code duplication with ChainQuery::stats()?
    pub fn stats(&self, scripthash: &[u8]) -> ScriptStats {
        let mut stats = ScriptStats::default();
        let mut seen_txids = HashSet::new();

        let entries = match self.history.get(scripthash) {
            None => return stats,
            Some(entries) => entries,
        };

        for entry in entries {
            if seen_txids.insert(get_entry_txid(entry)) {
                stats.tx_count += 1;
            }

            match entry {
                TxHistoryInfo::Funding(.., value) => {
                    stats.funded_txo_count += 1;
                    stats.funded_txo_sum += value;
                }
                TxHistoryInfo::Spending(.., value) => {
                    stats.spent_txo_count += 1;
                    stats.spent_txo_sum += value;
                }
            };
        }

        stats
    }

    pub fn update(&mut self, daemon: &Daemon) -> Result<()> {
        let new_txids = daemon
            .getmempooltxids()
            .chain_err(|| "failed to update mempool from daemon")?;
        let old_txids = HashSet::from_iter(self.txstore.keys().cloned());
        let to_remove: HashSet<&Sha256dHash> = old_txids.difference(&new_txids).collect();

        // Download and add new transactions from bitcoind's mempool
        let txids: Vec<&Sha256dHash> = new_txids.difference(&old_txids).collect();
        let to_add = match daemon.gettransactions(&txids) {
            Ok(txs) => txs,
            Err(err) => {
                warn!("failed to get transactions {:?}: {}", txids, err); // e.g. new block or RBF
                return Ok(()); // keep the mempool until next update()
            }
        };
        // Add new transactions
        self.add(to_add);
        // Remove missing transactions
        self.remove(to_remove);
        Ok(())
    }

    fn add(&mut self, txs: Vec<Transaction>) {
        let mut txids = vec![];
        // Phase 1: add to txstore
        for tx in txs {
            let txid = tx.txid();
            txids.push(txid);
            self.txstore.insert(txid, tx);
        }
        // Phase 2: index history and spend edges (can fail if some txos cannot be found)
        let txos = match self.lookup_txos(&self.get_prevouts(&txids)) {
            Ok(txos) => txos,
            Err(err) => {
                warn!("lookup txouts failed: {}", err);
                // TODO: should we remove txids from txstore?
                return;
            }
        };
        for txid in txids {
            let tx = self.txstore.get(&txid).expect("missing mempool tx");
            let txid_bytes = txid.into_bytes();

            let spending = tx.input.iter().enumerate().map(|(index, txi)| {
                let prev_txo = txos
                    .get(&txi.previous_output)
                    .expect(&format!("missing outpoint {:?}", txi.previous_output));
                (
                    compute_script_hash(&prev_txo.script_pubkey),
                    TxHistoryInfo::Spending(
                        txid_bytes,
                        index as u16,
                        txi.previous_output.txid.into_bytes(),
                        txi.previous_output.vout as u16,
                        prev_txo.value,
                    ),
                )
            });

            let funding = tx.output.iter().enumerate().map(|(index, txo)| {
                (
                    compute_script_hash(&txo.script_pubkey),
                    TxHistoryInfo::Funding(txid_bytes, index as u16, txo.value),
                )
            });

            for (scripthash, entry) in funding.chain(spending) {
                self.history
                    .entry(scripthash)
                    .or_insert_with(|| Vec::new())
                    .push(entry);
            }

            for (i, txi) in tx.input.iter().enumerate() {
                self.edges.insert(txi.previous_output, (txid, i as u32));
            }
        }
    }

    // @TODO use parallel lookup for confirmed txos?
    pub fn lookup_txos(&self, outpoints: &BTreeSet<OutPoint>) -> Result<HashMap<OutPoint, TxOut>> {
        outpoints
            .into_iter()
            .map(|outpoint| {
                let result = match self.txstore.get(&outpoint.txid) {
                    Some(txn) => txn.output.get(outpoint.vout as usize).cloned(),
                    // TODO: do concurrently for non-mempool txns
                    None => self.chain.lookup_txo(&outpoint),
                };
                match result {
                    Some(txout) => Ok((*outpoint, txout)),
                    None => bail!("missing outpoint {:?}", outpoint),
                }
            })
            .collect()
    }

    fn get_prevouts(&self, txids: &[Sha256dHash]) -> BTreeSet<OutPoint> {
        txids
            .iter()
            .map(|txid| self.txstore.get(&txid).expect("missing mempool tx"))
            .map(|tx| tx.input.iter().map(|txin| txin.previous_output))
            .flatten()
            .collect()
    }

    fn remove(&mut self, to_remove: HashSet<&Sha256dHash>) {
        for txid in &to_remove {
            self.txstore
                .remove(&txid)
                .expect(&format!("missing mempool tx {}", txid));
        }
        // TODO: make it more efficient (currently it takes O(|mempool|) time)
        self.history.retain(|_scripthash, entries| {
            entries.retain(|entry| !to_remove.contains(&get_entry_txid(entry)));
            !entries.is_empty()
        });

        self.edges
            .retain(|_outpoint, (txid, _vin)| !to_remove.contains(txid));
    }
}

fn get_entry_txid(entry: &TxHistoryInfo) -> Sha256dHash {
    match entry {
        TxHistoryInfo::Funding(txid, ..) => parse_hash(&txid),
        TxHistoryInfo::Spending(txid, ..) => parse_hash(&txid),
    }
}
