use crate::errors::*;
use crate::metrics::{Counter, MetricOpts, Metrics};
use bitcoin_hashes::sha256d::Hash as Sha256dHash;
use lru::LruCache;
use std::sync::Mutex;

pub struct BlockTxIDsCache {
    map: Mutex<LruCache<Sha256dHash /* blockhash */, Vec<Sha256dHash /* txid */>>>,
    hits: Counter,
    misses: Counter,
}

impl BlockTxIDsCache {
    pub fn new(capacity: usize, metrics: &Metrics) -> BlockTxIDsCache {
        BlockTxIDsCache {
            map: Mutex::new(LruCache::new(capacity)),
            hits: metrics.counter(MetricOpts::new(
                "electrs_blocktxids_cache_hits",
                "# of cache hits for list of transactions in a block",
            )),
            misses: metrics.counter(MetricOpts::new(
                "electrs_blocktxids_cache_misses",
                "# of cache misses for list of transactions in a block",
            )),
        }
    }

    pub fn get_or_else<F>(
        &self,
        blockhash: &Sha256dHash,
        load_txids_func: F,
    ) -> Result<Vec<Sha256dHash>>
    where
        F: FnOnce() -> Result<Vec<Sha256dHash>>,
    {
        if let Some(txids) = self.map.lock().unwrap().get(blockhash) {
            self.hits.inc();
            return Ok(txids.clone());
        }

        self.misses.inc();
        let txids = load_txids_func()?;
        self.map.lock().unwrap().put(*blockhash, txids.clone());
        Ok(txids)
    }
}
