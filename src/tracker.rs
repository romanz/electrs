use crate::bitcoin::{BlockHash, Txid};
use anyhow::{Context, Result};
use bindex::IndexedChain;
use bitcoin_slices::{bsl, Visit};

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
    ignore_mempool: bool,
}

impl Tracker {
    pub fn new(config: &Config, metrics: Metrics) -> Result<Self> {
        let index =
            IndexedChain::open(&config.db_dir, config.network).context("failed to open index")?;
        Ok(Self {
            index,
            mempool: Mempool::new(&metrics),
            ignore_mempool: config.ignore_mempool,
        })
    }

    pub(crate) fn headers(&self) -> &bindex::Headers {
        self.index.headers()
    }

    pub(crate) fn fees_histogram(&self) -> &FeeHistogram {
        self.mempool.fees_histogram()
    }

    pub(crate) fn get_unspent(&self, status: &ScriptHashStatus) -> Vec<UnspentEntry> {
        status.get_unspent()
    }

    pub(crate) fn sync(&mut self, daemon: &Daemon, exit_flag: &ExitFlag) -> Result<bool> {
        let stats = self.index.sync(1000)?;
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
            let tx_bytes = self.index.get_tx_bytes(&loc)?;
            if txid == compute_txid(&tx_bytes)? {
                return Ok(Some((loc.block_hash(), tx_bytes.into_boxed_slice())));
            }
        }
        Ok(None)
    }
}

fn compute_txid(tx_bytes: &[u8]) -> Result<Txid> {
    let mut visit = bitcoin_slices::EmptyVisitor {};
    let res = bsl::Transaction::visit(tx_bytes, &mut visit)
        .map_err(|err| anyhow!("invalid transaction: {:?}", err))?;
    ensure!(res.remaining().is_empty(), "non-empty remaining bytes");
    Ok(Txid::from_raw_hash(res.parsed().txid()))
}
