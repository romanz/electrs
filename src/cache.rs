use crate::errors::*;
use crate::metrics::{Counter, MetricOpts, Metrics};
use crate::rndcache::RndCache;
use bitcoin_hashes::sha256d::Hash as Sha256dHash;
use std::sync::Mutex;

pub struct BlockTxIDsCache {
    map: Mutex<RndCache<Sha256dHash /* blockhash */, Vec<Sha256dHash /* txid */>>>,
    hits: Counter,
    misses: Counter,
}

impl BlockTxIDsCache {
    pub fn new(capacity: usize, metrics: &Metrics) -> BlockTxIDsCache {
        BlockTxIDsCache {
            map: Mutex::new(RndCache::new(capacity)),
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
        let size_in_bytes = txids.len() * 32;
        let _ignored_err = self
            .map
            .lock()
            .unwrap()
            .put(*blockhash, txids.clone(), size_in_bytes);
        Ok(txids)
    }

    pub fn contains(&self, blockhash: &Sha256dHash) -> bool {
        self.map.lock().unwrap().get(blockhash).is_some()
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
        let capacity = txids.len() * 32 * 2; // two entries
        let cache = BlockTxIDsCache::new(capacity, &dummy_metrics);

        // cache miss
        let result = cache.get_or_else(&block1, &miss_func).unwrap();
        assert_eq!(1, *misses.lock().unwrap());
        assert_eq!(txids, result);

        // cache hit
        let result = cache.get_or_else(&block1, &miss_func).unwrap();
        assert_eq!(1, *misses.lock().unwrap());
        assert_eq!(txids, result);

        // cache size in this test fits two entries
        // .. first fill the cache
        cache.get_or_else(&block2, &miss_func).unwrap();
        assert_eq!(2, *misses.lock().unwrap());

        // .. add a third element, so one of the previous are evicted
        cache.get_or_else(&block3, &miss_func).unwrap();
        assert_eq!(3, *misses.lock().unwrap());
        // .. last added entry should not have been the one evicted
        assert!(cache.contains(&block3));

        // .. then verify that one (and only one) of the previous entries were
        // evicted
        let mut in_cache = 0;
        for b in vec![&block1, &block2].iter() {
            if cache.contains(b) {
                in_cache += 1;
            }
        }
        assert_eq!(1, in_cache)
    }

    #[test]
    /// Fetching items that don't fit in cache should work
    fn too_big_tx() {
        let dummy_metrics = Metrics::new("127.0.0.1:60000".parse().unwrap());
        let capacity = 1;

        let cache = BlockTxIDsCache::new(capacity, &dummy_metrics);

        let txids = vec![gen_hash(0), gen_hash(1)];
        let miss_func = || Ok(txids.clone());
        let block = gen_hash(2);
        let result = cache.get_or_else(&block, &miss_func).unwrap();
        assert_eq!(txids, result);
    }
}
