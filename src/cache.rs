use bitcoin::{BlockHash, Transaction, Txid};

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::merkle::Proof;

#[derive(Default)]
pub struct Cache {
    txs: Arc<RwLock<HashMap<Txid, Transaction>>>,
    proofs: Arc<RwLock<HashMap<(BlockHash, Txid), Proof>>>,
}

impl Cache {
    pub(crate) fn add_tx(&self, txid: Txid, f: impl FnOnce() -> Transaction) {
        self.txs.write().unwrap().entry(txid).or_insert_with(f);
    }

    pub(crate) fn get_tx<F, T>(&self, txid: &Txid, f: F) -> Option<T>
    where
        F: FnOnce(&Transaction) -> T,
    {
        self.txs.read().unwrap().get(txid).map(f)
    }

    pub(crate) fn add_proof<F>(&self, blockhash: BlockHash, txid: Txid, f: F)
    where
        F: FnOnce() -> Proof,
    {
        self.proofs
            .write()
            .unwrap()
            .entry((blockhash, txid))
            .or_insert_with(f);
    }

    pub(crate) fn get_proof<F, T>(&self, blockhash: BlockHash, txid: Txid, f: F) -> Option<T>
    where
        F: FnOnce(&Proof) -> T,
    {
        self.proofs.read().unwrap().get(&(blockhash, txid)).map(f)
    }
}
