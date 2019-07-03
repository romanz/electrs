use crate::errors::*;
use bitcoin_hashes::sha256d::Hash as Sha256dHash;
use lru::LruCache;
use std::sync::Mutex;

pub struct BlockTxIDsCache {
    map: Mutex<LruCache<Sha256dHash /* blockhash */, Vec<Sha256dHash /* txid */>>>,
}

impl BlockTxIDsCache {
    pub fn new(capacity: usize) -> BlockTxIDsCache {
        BlockTxIDsCache {
            map: Mutex::new(LruCache::new(capacity)),
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
            return Ok(txids.clone());
        }

        let txids = load_txids_func()?;
        self.map.lock().unwrap().put(*blockhash, txids.clone());
        Ok(txids)
    }
}
