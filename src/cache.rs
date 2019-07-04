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

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin_hashes::Hash;

    fn gen_hash(seed: u8) -> Sha256dHash {
        let bytes: Vec<u8> = (seed..seed + 32).collect();
        Sha256dHash::hash(&bytes[..])
    }

    #[test]
    fn test_cache_hit_and_miss() {
        let block1 = gen_hash(1);
        let block2 = gen_hash(2);
        let block3 = gen_hash(3);
        let txids = vec![gen_hash(4), gen_hash(5)];

        let misses: Mutex<usize> = Mutex::new(0);
        let miss_func = || {
            *misses.lock().unwrap() += 1;
            Ok(txids.clone())
        };

        let dummy_metrics = Metrics::new("127.0.0.1:60000".parse().unwrap());
        let cache = BlockTxIDsCache::new(2, &dummy_metrics);

        // cache miss
        let result = cache.get_or_else(&block1, &miss_func).unwrap();
        assert_eq!(1, *misses.lock().unwrap());
        assert_eq!(txids, result);

        // cache hit
        let result = cache.get_or_else(&block1, &miss_func).unwrap();
        assert_eq!(1, *misses.lock().unwrap());
        assert_eq!(txids, result);

        // cache size is 2, test that blockhash1 falls out of cache
        cache.get_or_else(&block2, &miss_func).unwrap();
        assert_eq!(2, *misses.lock().unwrap());
        cache.get_or_else(&block3, &miss_func).unwrap();
        assert_eq!(3, *misses.lock().unwrap());
        cache.get_or_else(&block1, &miss_func).unwrap();
        assert_eq!(4, *misses.lock().unwrap());

        // cache hits
        cache.get_or_else(&block3, &miss_func).unwrap();
        cache.get_or_else(&block1, &miss_func).unwrap();
        assert_eq!(4, *misses.lock().unwrap());
    }
}
