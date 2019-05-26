use bitcoin_hashes::sha256d::Hash as Sha256dHash;
use rayon::prelude::*;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use crate::chain::{OutPoint, Transaction, TxOut};
use crate::daemon::Daemon;
use crate::errors::*;
use crate::new_index::{ChainQuery, Mempool, ScriptStats, SpendingInput, Utxo};
use crate::util::{is_spendable, BlockId, Bytes, TransactionStatus};

#[cfg(feature = "liquid")]
use crate::elements::{lookup_asset, AssetEntry, AssetRegistry};

const FEE_ESTIMATES_TTL: u64 = 60; // seconds

const CONF_TARGETS: [u16; 9] = [
    2u16, 3u16, 4u16, 6u16, 10u16, 20u16, 144u16, 504u16, 1008u16,
];

pub struct Query {
    chain: Arc<ChainQuery>, // TODO: should be used as read-only
    mempool: Arc<RwLock<Mempool>>,
    daemon: Arc<Daemon>,
    cached_estimates: RwLock<Option<(HashMap<u16, f32>, Instant)>>,

    #[cfg(feature = "liquid")]
    asset_db: Option<AssetRegistry>,
}

impl Query {
    #[cfg(not(feature = "liquid"))]
    pub fn new(chain: Arc<ChainQuery>, mempool: Arc<RwLock<Mempool>>, daemon: Arc<Daemon>) -> Self {
        Query {
            chain,
            mempool,
            daemon,
            cached_estimates: RwLock::new(None),
        }
    }

    pub fn chain(&self) -> &ChainQuery {
        &self.chain
    }

    pub fn mempool(&self) -> RwLockReadGuard<Mempool> {
        self.mempool.read().unwrap()
    }

    pub fn broadcast_raw(&self, txhex: &String) -> Result<Sha256dHash> {
        let txid = self.daemon.broadcast_raw(&txhex)?;
        self.mempool
            .write()
            .unwrap()
            .add_by_txid(&self.daemon, &txid);
        Ok(txid)
    }

    pub fn utxo(&self, scripthash: &[u8]) -> Vec<Utxo> {
        let mut utxos = self.chain.utxo(scripthash);
        let mempool = self.mempool();
        utxos.retain(|utxo| !mempool.has_spend(&OutPoint::from(utxo)));
        utxos.extend(mempool.utxo(scripthash));
        utxos
    }

    pub fn history_txids(&self, scripthash: &[u8]) -> Vec<(Sha256dHash, Option<BlockId>)> {
        let confirmed_txids = self
            .chain
            .history_txids(scripthash)
            .into_iter()
            .map(|(tx, b)| (tx, Some(b)));

        let mempool_txids = self
            .mempool()
            .history_txids(scripthash)
            .into_iter()
            .map(|tx| (tx, None));

        confirmed_txids.chain(mempool_txids).collect()
    }

    pub fn stats(&self, scripthash: &[u8]) -> (ScriptStats, ScriptStats) {
        (
            self.chain.stats(scripthash),
            self.mempool().stats(scripthash),
        )
    }

    pub fn lookup_txn(&self, txid: &Sha256dHash) -> Option<Transaction> {
        self.chain
            .lookup_txn(txid)
            .or_else(|| self.mempool().lookup_txn(txid))
    }
    pub fn lookup_raw_txn(&self, txid: &Sha256dHash) -> Option<Bytes> {
        self.chain
            .lookup_raw_txn(txid)
            .or_else(|| self.mempool().lookup_raw_txn(txid))
    }

    pub fn lookup_txos(&self, outpoints: &BTreeSet<OutPoint>) -> HashMap<OutPoint, TxOut> {
        // the mempool lookup_txos() internally looks up confirmed txos as well
        self.mempool()
            .lookup_txos(outpoints)
            .expect("failed loading txos")
    }

    pub fn lookup_spend(&self, outpoint: &OutPoint) -> Option<SpendingInput> {
        self.chain
            .lookup_spend(outpoint)
            .or_else(|| self.mempool().lookup_spend(outpoint))
    }

    pub fn lookup_tx_spends(&self, tx: Transaction) -> Vec<Option<SpendingInput>> {
        let txid = tx.txid();

        tx.output
            .par_iter()
            .enumerate()
            .map(|(vout, txout)| {
                if is_spendable(txout) {
                    self.lookup_spend(&OutPoint {
                        txid,
                        vout: vout as u32,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn get_tx_status(&self, txid: &Sha256dHash) -> TransactionStatus {
        TransactionStatus::from(self.chain.tx_confirming_block(txid))
    }

    pub fn estimate_fee(&self, conf_target: u16) -> Option<f32> {
        self.estimate_fee_targets().remove(&conf_target)
    }

    pub fn estimate_fee_targets(&self) -> HashMap<u16, f32> {
        if let Some(ref cached) = *self.cached_estimates.read().unwrap() {
            if cached.1.elapsed() < Duration::from_secs(FEE_ESTIMATES_TTL) {
                return cached.0.clone();
            }
        }

        let fresh = CONF_TARGETS
            .iter()
            .filter_map(|conf_target| {
                self.daemon
                    .estimatesmartfee(*conf_target)
                    .ok()
                    .map(|feerate| (*conf_target, feerate))
            })
            .collect::<HashMap<u16, f32>>();

        *self.cached_estimates.write().unwrap() = Some((fresh.clone(), Instant::now()));

        fresh
    }

    #[cfg(feature = "liquid")]
    pub fn new(
        chain: Arc<ChainQuery>,
        mempool: Arc<RwLock<Mempool>>,
        daemon: Arc<Daemon>,
        asset_db: Option<AssetRegistry>,
    ) -> Self {
        Query {
            chain,
            mempool,
            daemon,
            asset_db,
            cached_estimates: RwLock::new(None),
        }
    }

    #[cfg(feature = "liquid")]
    pub fn lookup_asset(&self, asset_hash: &[u8]) -> Result<Option<AssetEntry>> {
        lookup_asset(&self.chain, self.asset_db.as_ref(), asset_hash)
    }
}
