use anyhow::{Context, Result};
use bitcoin::{consensus::Decodable, BlockHash, Transaction, Txid};

use crate::{
    cache::Cache,
    chain::Chain,
    config::Config,
    daemon::Daemon,
    db::DBStore,
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
    ignore_mempool: bool,
}

pub(crate) enum Error {
    NotReady,
}

impl Tracker {
    pub fn new(config: &Config, daemon: &Daemon, metrics: &Metrics) -> Result<Self> {
        let store = DBStore::open(&config.db_path, config.auto_reindex)?;
        let chain = Chain::new(daemon.get_genesis()?);
        Ok(Self {
            index: Index::load(
                store,
                chain,
                metrics,
                config.index_batch_size,
                config.index_lookup_limit,
                config.reindex_last_blocks,
            )
            .context("failed to open index")?,
            mempool: Mempool::new(metrics),
            ignore_mempool: config.ignore_mempool,
        })
    }

    pub(crate) fn chain(&self) -> &Chain {
        self.index.chain()
    }

    pub(crate) fn fees_histogram(&self) -> &FeeHistogram {
        self.mempool.fees_histogram()
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
        chain: &Chain,
        txid: Txid,
    ) -> Result<Option<(BlockHash, Transaction)>> {
        // Note: there are two blocks with coinbase transactions having same txid (see BIP-30)
        for pos in self.index.filter_by_txid(txid) {
            if let Some(row) = chain.get_header_row_for(pos) {
                let tx = Transaction::consensus_decode(&mut daemon.open_file(pos)?)?;
                if tx.txid() == txid {
                    return Ok(Some((row.hash, tx)));
                }
            }
        }
        Ok(None)
    }
}
