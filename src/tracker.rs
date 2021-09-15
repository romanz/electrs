use anyhow::{Context, Result};
use bitcoin::{BlockHash, Txid};

use std::path::Path;

use crate::{
    cache::Cache,
    chain::Chain,
    config::Config,
    daemon::Daemon,
    db::DBStore,
    index::Index,
    mempool::{Histogram, Mempool},
    metrics::Metrics,
    status::{Balance, HistoryEntry, ScriptHashStatus},
};

/// Electrum protocol subscriptions' tracker
pub struct Tracker {
    index: Index,
    mempool: Mempool,
    metrics: Metrics,
    index_batch_size: usize,
    ignore_mempool: bool,
}

impl Tracker {
    pub fn new(config: &Config) -> Result<Self> {
        let metrics = Metrics::new(config.monitoring_addr)?;
        let store = DBStore::open(Path::new(&config.db_path))?;
        let chain = Chain::new(config.network);
        Ok(Self {
            index: Index::load(store, chain, &metrics, config.index_lookup_limit)
                .context("failed to open index")?,
            mempool: Mempool::new(),
            metrics,
            index_batch_size: config.index_batch_size,
            ignore_mempool: config.ignore_mempool,
        })
    }

    pub(crate) fn chain(&self) -> &Chain {
        self.index.chain()
    }

    pub(crate) fn fees_histogram(&self) -> &Histogram {
        self.mempool.fees_histogram()
    }

    pub(crate) fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    pub(crate) fn get_history(&self, status: &ScriptHashStatus) -> Vec<HistoryEntry> {
        status.get_history(self.index.chain(), &self.mempool)
    }

    pub fn sync(&mut self, daemon: &Daemon) -> Result<()> {
        self.index.sync(daemon, self.index_batch_size)?;
        if !self.ignore_mempool {
            self.mempool.sync(daemon);
        }
        // TODO: double check tip - and retry on diff
        Ok(())
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

    pub fn get_blockhash_by_txid(&self, txid: Txid) -> Option<BlockHash> {
        // Note: there are two blocks with coinbase transactions having same txid (see BIP-30)
        self.index.filter_by_txid(txid).next()
    }
}
