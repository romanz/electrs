use anyhow::Result;
use bitcoin::{BlockHash, Transaction, Txid};
use electrs_rocksdb as rocksdb;
use parking_lot::RwLock;

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{
    metrics::{self, Histogram, Metrics},
    status::TxEntry,
    types::ScriptHash,
};

pub(crate) struct Cache {
    txs: Arc<RwLock<HashMap<Txid, Transaction>>>,
    db: Option<CacheDB>,

    // stats
    txs_size: Histogram,
}

impl Cache {
    pub fn new(metrics: &Metrics, cache_db_path: Option<&PathBuf>) -> Self {
        Cache {
            txs: Default::default(),
            db: cache_db_path.map(|path| CacheDB::open(path).unwrap()),
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
            self.txs_size.observe("serialized", tx.get_size() as f64);
            tx
        });
    }

    pub fn get_tx<F, T>(&self, txid: &Txid, f: F) -> Option<T>
    where
        F: FnOnce(&Transaction) -> T,
    {
        self.txs.read().get(txid).map(f)
    }

    pub fn add_status_entry(
        &self,
        scripthash: ScriptHash,
        blockhash: BlockHash,
        entries: &[TxEntry],
    ) {
        if let Some(db) = &self.db {
            db.add(scripthash, blockhash, entries);
        }
    }

    pub fn get_status_entries(&self, scripthash: ScriptHash) -> HashMap<BlockHash, Vec<TxEntry>> {
        self.db
            .as_ref()
            .map(|db| db.scan(scripthash))
            .unwrap_or_default()
    }
}

struct CacheDB {
    db: rocksdb::DB,
}

impl CacheDB {
    fn open(path: &Path) -> Result<Self> {
        let db = rocksdb::DB::open_default(path)?;
        let live_files = db.live_files()?;
        info!(
            "{:?}: {} SST files, {} GB, {} Grows",
            path,
            live_files.len(),
            live_files.iter().map(|f| f.size).sum::<usize>() as f64 / 1e9,
            live_files.iter().map(|f| f.num_entries).sum::<u64>() as f64 / 1e9
        );
        Ok(CacheDB { db })
    }

    fn add(&self, scripthash: ScriptHash, blockhash: BlockHash, entries: &[TxEntry]) {
        let mut cursor = Cursor::new(Vec::with_capacity(1024));
        cursor.write_all(&scripthash).unwrap();
        bincode::serialize_into(&mut cursor, &blockhash).unwrap();
        bincode::serialize_into(&mut cursor, entries).unwrap();
        let mut batch = rocksdb::WriteBatch::default();
        batch.put(cursor.into_inner(), b"");
        self.db.write_without_wal(batch).unwrap(); // best-effort write
    }

    fn scan(&self, scripthash: ScriptHash) -> HashMap<BlockHash, Vec<TxEntry>> {
        let mode = rocksdb::IteratorMode::From(&scripthash, rocksdb::Direction::Forward);
        self.db
            .iterator(mode)
            .map(|(key, _)| key)
            .take_while(|key| key.starts_with(&scripthash))
            .map(|key| {
                let mut cursor = &key[scripthash.len()..];
                let blockhash: BlockHash = bincode::deserialize_from(&mut cursor).unwrap();
                let entries: Vec<TxEntry> = bincode::deserialize_from(&mut cursor).unwrap();
                (blockhash, entries)
            })
            .collect()
    }
}
