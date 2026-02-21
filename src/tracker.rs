use std::ops::ControlFlow;
use std::path::Path;

use anyhow::{Context, Result};
use bitcoin::{BlockHash, Txid};
use bitcoin_slices::{bsl, Error::VisitBreak, Visit, Visitor};

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
    types::bsl_txid,
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

/// Finds the highest block height for which txid, funding, and spending CDB files all exist.
fn find_highest_finalized_cdb_height(cdb_path: &Path) -> Result<Option<usize>> {
    let entries = match std::fs::read_dir(cdb_path) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    let mut max_height: Option<usize> = None;
    for entry in entries {
        let entry =
            entry.with_context(|| format!("failed to read CDB directory {:?}", cdb_path))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(suffix) = name
            .strip_prefix("txid")
            .and_then(|s| s.strip_suffix(".cdb"))
        else {
            continue;
        };
        let Ok(height) = suffix.parse::<usize>() else {
            continue;
        };
        let funding = cdb_path.join(format!("funding{}.cdb", height));
        let spending = cdb_path.join(format!("spending{}.cdb", height));
        if funding.is_file() && spending.is_file() {
            if max_height.map_or(true, |m| height > m) {
                max_height = Some(height);
            }
        }
    }
    Ok(max_height)
}

impl Tracker {
    pub fn new(config: &Config, metrics: Metrics) -> Result<Self> {
        if let Some(cdb_path) = &config.cdb_path {
            std::fs::create_dir_all(cdb_path)
                .with_context(|| format!("failed to create CDB directory {:?}", cdb_path))?;
        }
        let mut store = DBStore::open(
            &config.db_path,
            config.db_log_dir.as_deref(),
            config.auto_reindex,
            config.db_parallelism,
        )?;
        if let Some(cdb_path) = &config.cdb_path {
            if let Some(height) = find_highest_finalized_cdb_height(cdb_path)? {
                store.open_finalized_cdb(cdb_path, height)?;
            }
        }
        let chain = Chain::new(config.network);
        Ok(Self {
            index: Index::load(
                store,
                chain,
                &metrics,
                config.index_batch_size,
                config.index_lookup_limit,
                config.reindex_last_blocks,
                config.cdb_path.clone(),
                config.cdb_max_block_height,
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
            self.mempool.sync(daemon, exit_flag);
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
    ) -> Result<Option<(BlockHash, Box<[u8]>)>> {
        // Note: there are two blocks with coinbase transactions having same txid (see BIP-30)
        let blockhashes = self.index.filter_by_txid(txid);
        let mut result = None;
        daemon.for_blocks(blockhashes, |blockhash, block| {
            if result.is_some() {
                return; // keep first matching transaction
            }
            let mut visitor = FindTransaction::new(txid);
            result = match bsl::Block::visit(&block, &mut visitor) {
                Ok(_) | Err(VisitBreak) => visitor.found.map(|tx| (blockhash, tx)),
                Err(e) => panic!("core returned invalid block: {:?}", e),
            };
        })?;
        Ok(result)
    }
}

pub struct FindTransaction {
    txid: bitcoin::Txid,
    found: Option<Box<[u8]>>, // no need to deserialize
}

impl FindTransaction {
    pub fn new(txid: bitcoin::Txid) -> Self {
        Self { txid, found: None }
    }
}
impl Visitor for FindTransaction {
    fn visit_transaction(&mut self, tx: &bsl::Transaction) -> ControlFlow<()> {
        if self.txid == bsl_txid(tx) {
            self.found = Some(tx.as_ref().into());
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }
}
