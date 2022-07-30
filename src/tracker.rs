use anyhow::{Context, Result};
use bitcoin::{BlockHash, Transaction, Txid};

use crate::{
    cache::Cache,
    chain::Chain,
    config::Config,
    daemon::Daemon,
    db_store::DBStore,
    index::Index,
    mempool::{FeeHistogram, Mempool},
    metrics::Metrics,
    signals::ExitFlag,
    status::{Balance, ScriptHashStatus, UnspentEntry},
};

/// Electrum protocol subscriptions' tracker
pub struct Tracker {
    index: Index,
    mempool: Mempool,
    metrics: Metrics,
    ignore_mempool: bool,
}

pub(crate) enum Error {
    NotReady,
}

#[cfg(feature = "lmdb-rkv")]
fn get_db_store(config: &Config) -> Result<Box<dyn DBStore + Send + Sync + 'static>> {
    Ok(Box::new(crate::db_lmdb_rkv::LmdbStore::open(
        &config.db_path,
        config.auto_reindex,
    )?))
}

#[cfg(feature = "rocksdb")]
fn get_db_store(config: &Config) -> Result<Box<dyn DBStore + Send + Sync + 'static>> {
    Ok(Box::new(crate::db_rocksdb::RocksDBStore::open(
        &config.db_path,
        config.auto_reindex,
    )?))
}

#[cfg(feature = "sled")]
fn get_db_store(config: &Config) -> Result<Box<dyn DBStore + Send + Sync + 'static>> {
    Ok(Box::new(crate::db_sled::SledStore::open(
        &config.db_path,
        config.auto_reindex,
    )?))
}

#[cfg(not(any(feature = "lmdb-rkv", feature = "sled", feature = "rocksdb")))]
fn get_db_store(config: &Config) -> Result<Box<dyn DBStore + Send + Sync + 'static>> {
    panic!("Must choose exactly one backing store in the features: rocksdb_store, sled_store");
}

impl Tracker {
    pub fn new(config: &Config, metrics: Metrics) -> Result<Self> {
        let store: Box<dyn DBStore + Send + Sync + 'static> = get_db_store(config)?;

        let chain = Chain::new(config.network);
        Ok(Self {
            index: Index::load(
                store,
                chain,
                &metrics,
                config.index_batch_size,
                config.index_lookup_limit,
                config.reindex_last_blocks,
            )
            .context("failed to open index")?,
            mempool: Mempool::new(&metrics),
            metrics,
            ignore_mempool: config.ignore_mempool,
        })
    }

    pub(crate) fn chain(&self) -> &Chain {
        self.index.chain()
    }

    pub(crate) fn fees_histogram(&self) -> &FeeHistogram {
        self.mempool.fees_histogram()
    }

    pub(crate) fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    pub(crate) fn get_unspent(&self, status: &ScriptHashStatus) -> Vec<UnspentEntry> {
        status.get_unspent(self.index.chain())
    }

    pub(crate) fn sync(&mut self, daemon: &Daemon, exit_flag: &ExitFlag) -> Result<bool> {
        let done = self.index.sync(daemon, exit_flag)?;
        if done && !self.ignore_mempool {
            self.mempool.sync(daemon);
            // TODO: double check tip - and retry on diff
        }
        Ok(done)
    }

    pub(crate) fn status(&self) -> Result<(), Error> {
        if self.index.is_ready() {
            return Ok(());
        }
        Err(Error::NotReady)
    }

    pub(crate) fn update_scripthash_status(
        &self,
        status: &mut ScriptHashStatus,
        daemon: &Daemon,
        cache: &Cache,
    ) -> Result<bool> {
        let prev_statushash = status.statushash();
        status.sync(&self.index, &self.mempool, daemon, cache)?;
        Ok(prev_statushash != status.statushash())
    }

    pub(crate) fn get_balance(&self, status: &ScriptHashStatus) -> Balance {
        status.get_balance(self.chain())
    }

    pub(crate) fn lookup_transaction(
        &self,
        daemon: &Daemon,
        txid: Txid,
    ) -> Result<Option<(BlockHash, Transaction)>> {
        // Note: there are two blocks with coinbase transactions having same txid (see BIP-30)
        let blockhashes = self.index.filter_by_txid(txid);
        let mut result = None;
        daemon.for_blocks(blockhashes, |blockhash, block| {
            for tx in block.txdata {
                if result.is_some() {
                    return;
                }
                if tx.txid() == txid {
                    result = Some((blockhash, tx));
                    return;
                }
            }
        })?;
        Ok(result)
    }
}
