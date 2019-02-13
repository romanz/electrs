use bitcoin::util::hash::Sha256dHash;
use rayon::prelude::*;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::chain::{OutPoint, Transaction, TxOut};
use crate::new_index::{ChainQuery, Mempool, ScriptStats, SpendingInput, Utxo};
use crate::util::{is_spendable, Bytes, TransactionStatus};

pub struct Query {
    chain: Arc<ChainQuery>, // TODO: should be used as read-only
    mempool: Arc<RwLock<Mempool>>,
}

impl Query {
    pub fn new(chain: Arc<ChainQuery>, mempool: Arc<RwLock<Mempool>>) -> Self {
        Query { chain, mempool }
    }

    pub fn chain(&self) -> &ChainQuery {
        &self.chain
    }

    pub fn mempool(&self) -> RwLockReadGuard<Mempool> {
        self.mempool.read().unwrap()
    }

    pub fn mempool_write(&self) -> RwLockWriteGuard<Mempool> {
        self.mempool.write().unwrap()
    }

    pub fn utxo(&self, scripthash: &[u8]) -> Vec<Utxo> {
        let mut utxos = self.chain.utxo(scripthash);
        let mempool = self.mempool();
        utxos.retain(|utxo| !mempool.has_spend(&OutPoint::from(utxo)));
        utxos.extend(mempool.utxo(scripthash));
        utxos
    }

    pub fn stats(&self, scripthash: &[u8]) -> (ScriptStats, ScriptStats) {
        (
            self.chain.stats(scripthash),
            self.mempool().stats(scripthash),
        )
    }

    pub fn lookup_txn(&self, txid: &Sha256dHash) -> Option<Transaction> {
        self.chain
            .lookup_txn(txid)
            .or_else(|| self.mempool().lookup_txn(txid))
    }
    pub fn lookup_raw_txn(&self, txid: &Sha256dHash) -> Option<Bytes> {
        self.chain
            .lookup_raw_txn(txid)
            .or_else(|| self.mempool().lookup_raw_txn(txid))
    }

    pub fn lookup_txos(&self, outpoints: &BTreeSet<OutPoint>) -> HashMap<OutPoint, TxOut> {
        // the mempool lookup_txos() internally looks up confirmed txos as well
        self.mempool()
            .lookup_txos(outpoints)
            .expect("failed loading txos")
    }

    pub fn lookup_spend(&self, outpoint: &OutPoint) -> Option<SpendingInput> {
        self.chain
            .lookup_spend(outpoint)
            .or_else(|| self.mempool().lookup_spend(outpoint))
    }

    pub fn lookup_tx_spends(&self, tx: Transaction) -> Vec<Option<SpendingInput>> {
        let txid = tx.txid();

        tx.output
            .par_iter()
            .enumerate()
            .map(|(vout, txout)| {
                if is_spendable(txout) {
                    self.lookup_spend(&OutPoint {
                        txid,
                        vout: vout as u32,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn get_tx_status(&self, txid: &Sha256dHash) -> TransactionStatus {
        TransactionStatus::from(self.chain.tx_confirming_block(txid))
    }
}
