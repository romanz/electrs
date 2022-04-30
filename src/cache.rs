use bitcoin::{Transaction, Txid};
use parking_lot::RwLock;

use std::collections::HashMap;
use std::sync::Arc;

use crate::metrics::{self, Histogram, Metrics};

pub(crate) struct Cache {
    txs: Arc<RwLock<HashMap<Txid, Transaction>>>,

    // stats
    txs_size: Histogram,
}

impl Cache {
    pub fn new(metrics: &Metrics) -> Self {
        Cache {
            txs: Default::default(),
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
            self.txs_size.observe("serialized", tx.size() as f64);
            tx
        });
    }

    pub fn get_tx<F, T>(&self, txid: &Txid, f: F) -> Option<T>
    where
        F: FnOnce(&Transaction) -> T,
    {
        self.txs.read().get(txid).map(f)
    }
}
