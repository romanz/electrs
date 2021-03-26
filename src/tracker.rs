use anyhow::{Context, Result};
use bitcoin::{BlockHash, Txid};
use serde_json::Value;

use std::convert::TryInto;
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
    status::Status,
};

/// Electrum protocol subscriptions' tracker
pub struct Tracker {
    index: Index,
    mempool: Mempool,
    metrics: Metrics,
    index_batch_size: usize,
}

impl Tracker {
    pub fn new(config: &Config) -> Result<Self> {
        let metrics = Metrics::new(config.monitoring_addr)?;
        let store = DBStore::open(Path::new(&config.db_path))?;
        let chain = Chain::new(config.network);
        Ok(Self {
            index: Index::load(store, chain, &metrics).context("failed to open index")?,
            mempool: Mempool::new(),
            metrics,
            index_batch_size: config.index_batch_size,
        })
    }

    pub(crate) fn chain(&self) -> &Chain {
        self.index.chain()
    }

    pub(crate) fn fees_histogram(&self) -> &Histogram {
        &self.mempool.fees_histogram()
    }

    pub(crate) fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    pub fn get_history(&self, status: &Status) -> impl Iterator<Item = Value> {
        let confirmed = status
            .get_confirmed(&self.index.chain())
            .into_iter()
            .map(|entry| entry.value());
        let mempool = status
            .get_mempool(&self.mempool)
            .into_iter()
            .map(|entry| entry.value());
        confirmed.chain(mempool)
    }

    pub fn sync(&mut self, daemon: &Daemon) -> Result<()> {
        self.index.sync(daemon, self.index_batch_size)?;
        self.mempool.sync(daemon)?;
        // TODO: double check tip - and retry on diff
        Ok(())
    }

    pub fn update_status(
        &self,
        status: &mut Status,
        daemon: &Daemon,
        cache: &Cache,
    ) -> Result<bool> {
        let prev_statushash = status.statushash();
        status.sync(&self.index, &self.mempool, daemon, cache)?;
        Ok(prev_statushash != status.statushash())
    }

    pub fn get_balance(&self, status: &Status, cache: &Cache) -> bitcoin::Amount {
        let unspent = status.get_unspent(&self.index.chain());
        let mut balance = bitcoin::Amount::ZERO;
        for outpoint in &unspent {
            let value = cache
                .get_tx(&outpoint.txid, |tx| {
                    let vout: usize = outpoint.vout.try_into().unwrap();
                    bitcoin::Amount::from_sat(tx.output[vout].value)
                })
                .expect("missing tx");
            balance += value;
        }
        balance
    }

    pub fn get_blockhash_by_txid(&self, txid: Txid) -> Option<BlockHash> {
        // Note: there are two blocks with coinbase transactions having same txid (see BIP-30)
        self.index.filter_by_txid(txid).next()
    }
}
