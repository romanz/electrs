use crate::bitcoin::{consensus::Decodable, BlockHash, Transaction, Txid};
use anyhow::{Context, Result};
use bindex::IndexedChain;

use crate::{
    config::Config,
    daemon::Daemon,
    mempool::{FeeHistogram, Mempool},
    metrics::Metrics,
    signals::ExitFlag,
    status::{Balance, ScriptHashStatus, UnspentEntry},
};

/// Electrum protocol subscriptions' tracker
pub struct Tracker {
    index: IndexedChain,
    mempool: Mempool,
    metrics: Metrics,
    ignore_mempool: bool,
}

impl Tracker {
    pub fn new(config: &Config, metrics: Metrics) -> Result<Self> {
        let index = IndexedChain::open(&config.db_dir, config.network.into())
            .context("failed to open index")?;
        Ok(Self {
            index,
            mempool: Mempool::new(&metrics),
            metrics,
            ignore_mempool: config.ignore_mempool,
        })
    }

    pub(crate) fn headers(&self) -> &bindex::Headers {
        self.index.headers()
    }

    pub(crate) fn fees_histogram(&self) -> &FeeHistogram {
        self.mempool.fees_histogram()
    }

    pub(crate) fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    pub(crate) fn get_unspent(&self, status: &ScriptHashStatus) -> Vec<UnspentEntry> {
        status.get_unspent()
    }

    pub(crate) fn sync(&mut self, daemon: &Daemon, exit_flag: &ExitFlag) -> Result<bool> {
        let stats = self.index.sync_chain(1000)?;
        let done = stats.indexed_blocks == 0;
        if done && !self.ignore_mempool {
            self.mempool.sync(daemon, exit_flag);
            // TODO: double check tip - and retry on diff
        }
        Ok(done)
    }

    pub(crate) fn status(&self) -> Result<()> {
        Ok(())
    }

    pub(crate) fn update_scripthash_status(&self, status: &mut ScriptHashStatus) -> Result<bool> {
        let prev_statushash = status.statushash();
        status.sync(&self.index, &self.mempool)?;
        Ok(prev_statushash != status.statushash())
    }

    pub(crate) fn get_balance(&self, status: &ScriptHashStatus) -> Balance {
        status.get_balance()
    }

    pub(crate) fn lookup_transaction(&self, txid: Txid) -> Result<Option<(BlockHash, Box<[u8]>)>> {
        // Note: there are two blocks with coinbase transactions having same txid (see BIP-30)
        for loc in self.index.locations_by_txid(&txid)? {
            let tx_bytes = self.index.get_tx_bytes(&loc)?.into_boxed_slice();
            let tx = Transaction::consensus_decode_from_finite_reader(&mut &tx_bytes[..])?;
            if tx.compute_txid() == txid {
                return Ok(Some((loc.block_hash(), tx_bytes)));
            }
        }
        Ok(None)
    }
}
