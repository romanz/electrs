use bitcoin::{BlockHash, Transaction, Txid};
use parking_lot::RwLock;

use std::collections::HashMap;
use std::sync::Arc;

use crate::{
    merkle::Proof,
    metrics::{self, Histogram, Metrics},
};

pub(crate) struct Cache {
    txs: Arc<RwLock<HashMap<Txid, Transaction>>>,
    proofs: Arc<RwLock<HashMap<(BlockHash, Txid), Proof>>>,

    // stats
    txs_size: Histogram,
}

impl Cache {
    pub fn new(metrics: &Metrics) -> Self {
        Cache {
            txs: Default::default(),
            proofs: Default::default(),
            txs_size: metrics.histogram_vec(
                "cache_txs_size",
                "Cached transactions' size (in bytes)",
                "type",
                metrics::default_size_buckets(),
            ),
        }
    }

    pub fn add_tx(&self, txid: Txid, f: impl FnOnce() -> Transaction) {
        self.txs.write().entry(txid).or_insert_with(|| {
            let tx = f();
            self.txs_size.observe("serialized", tx.get_size());
            tx
        });
    }

    pub fn get_tx<F, T>(&self, txid: &Txid, f: F) -> Option<T>
    where
        F: FnOnce(&Transaction) -> T,
    {
        self.txs.read().get(txid).map(f)
    }

    pub fn add_proof(&self, blockhash: BlockHash, txid: Txid, proof: Proof) {
        self.proofs
            .write()
            .entry((blockhash, txid))
            .or_insert(proof);
    }

    pub fn get_proof<F, T>(&self, blockhash: BlockHash, txid: Txid, f: F) -> Option<T>
    where
        F: FnOnce(&Proof) -> T,
    {
        self.proofs.read().get(&(blockhash, txid)).map(f)
    }
}
