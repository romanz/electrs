use bincode;
use bitcoin::blockdata::script::Script;
use bitcoin::consensus::encode::{deserialize, serialize};
use bitcoin::util::hash::Sha256dHash;
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use itertools::Itertools;
use rayon::prelude::*;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, RwLock};

use crate::chain::{BlockHeader, OutPoint, Transaction, TxOut, Value};
use crate::daemon::Daemon;
use crate::errors::*;
use crate::metrics::{HistogramOpts, HistogramTimer, HistogramVec, Metrics};
use crate::util::{
    full_hash, has_prevout, is_spendable, BlockHeaderMeta, BlockId, BlockMeta, BlockStatus, Bytes,
    HeaderEntry, HeaderList,
};

use crate::new_index::db::{DBFlush, DBRow, ReverseScanIterator, ScanIterator, DB};
use crate::new_index::fetch::{start_fetcher, BlockEntry, FetchFrom};

const MIN_HISTORY_ITEMS_TO_CACHE: usize = 10; // TODO: number TBD

pub struct Store {
    // TODO: should be column families
    txstore_db: DB,
    history_db: DB,
    cache_db: DB,
    added_blockhashes: RwLock<HashSet<Sha256dHash>>,
    indexed_blockhashes: RwLock<HashSet<Sha256dHash>>,
    indexed_headers: RwLock<HeaderList>,
}

impl Store {
    pub fn open(path: &Path) -> Self {
        let txstore_db = DB::open(&path.join("txstore"));
        let added_blockhashes = load_blockhashes(&txstore_db, &BlockRow::done_filter());
        debug!("{} blocks were added", added_blockhashes.len());
        let history_db = DB::open(&path.join("history"));
        let indexed_blockhashes = load_blockhashes(&history_db, &BlockRow::done_filter());
        debug!("{} blocks were indexed", indexed_blockhashes.len());
        let headers_map = load_blockheaders(&txstore_db);
        debug!("{} headers were loaded", headers_map.len());
        let cache_db = DB::open(&path.join("cache"));
        Store {
            txstore_db,
            history_db,
            cache_db,
            added_blockhashes: RwLock::new(added_blockhashes),
            indexed_blockhashes: RwLock::new(indexed_blockhashes),
            indexed_headers: RwLock::new(HeaderList::empty()),
        }
    }

    /// Used to decide whether to use "blk*.dat" files for faster initial indexing
    pub fn is_empty(&self) -> bool {
        return self.added_blockhashes.read().unwrap().is_empty();
    }
}

type UtxoMap = HashMap<OutPoint, (BlockId, Value)>;

#[derive(Debug)]
pub struct Utxo {
    pub txid: Sha256dHash,
    pub vout: u32,
    pub confirmed: Option<BlockId>,
    pub value: Value,
}

impl From<&Utxo> for OutPoint {
    fn from(utxo: &Utxo) -> Self {
        OutPoint {
            txid: utxo.txid,
            vout: utxo.vout,
        }
    }
}

#[derive(Debug)]
pub struct SpendingInput {
    pub txid: Sha256dHash,
    pub vin: u32,
    pub confirmed: Option<BlockId>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ScriptStats {
    pub tx_count: usize,
    pub funded_txo_count: usize,
    pub spent_txo_count: usize,
    #[cfg(not(feature = "liquid"))]
    pub funded_txo_sum: u64,
    #[cfg(not(feature = "liquid"))]
    pub spent_txo_sum: u64,
}

impl ScriptStats {
    pub fn default() -> Self {
        ScriptStats {
            tx_count: 0,
            funded_txo_count: 0,
            spent_txo_count: 0,
            #[cfg(not(feature = "liquid"))]
            funded_txo_sum: 0,
            #[cfg(not(feature = "liquid"))]
            spent_txo_sum: 0,
        }
    }
}

pub struct Indexer {
    store: Arc<Store>,
    flush: DBFlush,
    from: FetchFrom,
    duration: HistogramVec,
}

pub struct ChainQuery {
    store: Arc<Store>, // TODO: should be used as read-only
    duration: HistogramVec,
}

// TODO: &[Block] should be an iterator / a queue.
impl Indexer {
    pub fn open(store: Arc<Store>, from: FetchFrom, metrics: &Metrics) -> Self {
        Indexer {
            store,
            flush: DBFlush::Disable,
            from,
            duration: metrics.histogram_vec(
                HistogramOpts::new("index_duration", "Index update duration (in seconds)"),
                &["step"],
            ),
        }
    }

    fn start_timer(&self, name: &str) -> HistogramTimer {
        self.duration.with_label_values(&[name]).start_timer()
    }

    fn headers_to_add(&self, new_headers: &[HeaderEntry]) -> Vec<HeaderEntry> {
        let added_blockhashes = self.store.added_blockhashes.read().unwrap();
        new_headers
            .iter()
            .filter(|e| !added_blockhashes.contains(e.hash()))
            .cloned()
            .collect()
    }

    fn headers_to_index(&self, new_headers: &[HeaderEntry]) -> Vec<HeaderEntry> {
        let indexed_blockhashes = self.store.indexed_blockhashes.read().unwrap();
        new_headers
            .iter()
            .filter(|e| !indexed_blockhashes.contains(e.hash()))
            .cloned()
            .collect()
    }

    fn start_auto_compactions(&self, db: &DB) {
        let key = b"F".to_vec();
        if db.get(&key).is_none() {
            db.full_compaction();
            db.put(&key, b"");
            assert!(db.get(&key).is_some());
        }
        db.enable_auto_compaction();
    }

    fn get_new_headers(&self, daemon: &Daemon, tip: &Sha256dHash) -> Result<Vec<HeaderEntry>> {
        let headers = self.store.indexed_headers.read().unwrap();
        let new_headers = daemon.get_new_headers(&headers, &tip)?;
        let result = headers.order(new_headers);
        result.last().map(|tip| {
            info!("{:?} ({} left to process)", tip, result.len());
        });
        Ok(result)
    }

    pub fn update(&mut self, daemon: &Daemon) -> Result<Sha256dHash> {
        let daemon = daemon.reconnect()?;
        let tip = daemon.getbestblockhash()?;
        let new_headers = self.get_new_headers(&daemon, &tip)?;

        let to_add = self.headers_to_add(&new_headers);
        debug!(
            "adding transactions from {} blocks using {:?}",
            to_add.len(),
            self.from
        );
        start_fetcher(self.from, &daemon, to_add)?.map(|blocks| self.add(&blocks));
        self.start_auto_compactions(&self.store.txstore_db);

        let to_index = self.headers_to_index(&new_headers);
        debug!(
            "indexing history from {} blocks using {:?}",
            to_index.len(),
            self.from
        );
        start_fetcher(self.from, &daemon, to_index)?.map(|blocks| self.index(&blocks));
        self.start_auto_compactions(&self.store.history_db);

        let mut headers = self.store.indexed_headers.write().unwrap();
        headers.apply(new_headers);
        assert_eq!(tip, *headers.tip());

        self.flush = DBFlush::Enable;
        self.store.txstore_db.write(vec![], self.flush);
        self.store.history_db.write(vec![], self.flush);
        self.from = FetchFrom::Bitcoind;
        Ok(tip)
    }

    fn add(&self, blocks: &[BlockEntry]) {
        // TODO: skip orphaned blocks?
        let rows = {
            let _timer = self.start_timer("add_process");
            add_blocks(blocks)
        };
        {
            let _timer = self.start_timer("add_write");
            self.store.txstore_db.write(rows, self.flush);
        }

        self.store
            .added_blockhashes
            .write()
            .unwrap()
            .extend(blocks.into_iter().map(|b| b.entry.hash()));
    }

    fn index(&self, blocks: &[BlockEntry]) {
        let previous_txos_map = {
            let _timer = self.start_timer("index_lookup");
            lookup_txos(&self.store.txstore_db, &get_previous_txos(blocks), false)
        };
        let rows = {
            let _timer = self.start_timer("index_process");
            let added_blockhashes = self.store.added_blockhashes.read().unwrap();
            for b in blocks {
                let blockhash = b.entry.hash();
                // TODO: replace by lookup into txstore_db?
                if !added_blockhashes.contains(&blockhash) {
                    panic!("cannot index block {} (missing from store)", blockhash);
                }
            }
            index_blocks(blocks, &previous_txos_map)
        };
        self.store.history_db.write(rows, self.flush);
    }
}

impl ChainQuery {
    pub fn new(store: Arc<Store>, metrics: &Metrics) -> Self {
        ChainQuery {
            store,
            duration: metrics.histogram_vec(
                HistogramOpts::new("query_duration", "Index query duration (in seconds)"),
                &["name"],
            ),
        }
    }

    fn start_timer(&self, name: &str) -> HistogramTimer {
        self.duration.with_label_values(&[name]).start_timer()
    }

    pub fn get_block_txids(&self, hash: &Sha256dHash) -> Option<Vec<Sha256dHash>> {
        let _timer = self.start_timer("get_block_txids");
        self.store
            .txstore_db
            .get(&BlockRow::txids_key(hash.to_bytes()))
            .map(|val| bincode::deserialize(&val).expect("failed to parse block txids"))
    }

    pub fn get_block_meta(&self, hash: &Sha256dHash) -> Option<BlockMeta> {
        let _timer = self.start_timer("get_block_meta");
        self.store
            .txstore_db
            .get(&BlockRow::meta_key(hash.to_bytes()))
            .map(|val| bincode::deserialize(&val).expect("failed to parse BlockMeta"))
    }

    pub fn get_block_with_meta(&self, hash: &Sha256dHash) -> Option<BlockHeaderMeta> {
        let _timer = self.start_timer("get_block_with_meta");
        Some(BlockHeaderMeta {
            header_entry: self.header_by_hash(hash)?,
            meta: self.get_block_meta(hash)?,
        })
    }

    fn history_iter_scan(&self, scripthash: &[u8], start_height: usize) -> ScanIterator {
        self.store.history_db.iter_scan_from(
            &TxHistoryRow::filter(&scripthash[..]),
            &TxHistoryRow::prefix_height(&scripthash[..], start_height as u32),
        )
    }
    fn history_iter_scan_reverse(&self, scripthash: &[u8]) -> ReverseScanIterator {
        self.store.history_db.iter_scan_reverse(
            &TxHistoryRow::filter(&scripthash[..]),
            &TxHistoryRow::prefix_end(&scripthash[..]),
        )
    }

    pub fn history(
        &self,
        scripthash: &[u8],
        last_seen_txid: Option<&Sha256dHash>,
        limit: usize,
    ) -> Vec<(Transaction, Option<BlockId>)> {
        let _timer_scan = self.start_timer("history");
        let txs_conf = self
            .history_iter_scan_reverse(scripthash)
            .map(|row| TxHistoryRow::from_row(row).get_txid())
            // XXX: unique() requires keeping an in-memory list of all txids, can we avoid that?
            .unique()
            // TODO seek directly to last seen tx without reading earlier rows
            .skip_while(|txid| {
                // skip until we reach the last_seen_txid
                last_seen_txid.map_or(false, |last_seen_txid| last_seen_txid != txid)
            })
            .skip(match last_seen_txid {
                Some(_) => 1, // skip the last_seen_txid itself
                None => 0,
            })
            .filter_map(|txid| self.tx_confirming_block(&txid).map(|b| (txid, b)))
            .take(limit)
            .collect::<Vec<(Sha256dHash, BlockId)>>();

        let txids = txs_conf.iter().map(|t| t.0.clone()).collect();
        self.lookup_txns(&txids)
            .expect("failed looking up txs in history index")
            .into_iter()
            .zip(txs_conf)
            .map(|(tx, (_, blockid))| (tx, Some(blockid)))
            .collect()
    }

    // TODO: avoid duplication with stats/stats_delta?
    pub fn utxo(&self, scripthash: &[u8]) -> Vec<Utxo> {
        let _timer = self.start_timer("utxo");

        // get the last known utxo set and the blockhash it was updated for.
        // invalidates the cache if the block was orphaned.
        let cache: Option<(UtxoMap, usize)> = self
            .store
            .cache_db
            .get(&UtxoCacheRow::key(scripthash))
            .map(|c| bincode::deserialize(&c).unwrap())
            .and_then(|(utxos_cache, blockhash)| {
                self.height_by_hash(&blockhash)
                    .map(|height| (utxos_cache, height))
            })
            .map(|(utxos_cache, height)| (UtxoCacheRow::to_utxo_map(utxos_cache, self), height));
        let had_cache = cache.is_some();

        // update utxo set with new transactions since
        let (newutxos, lastblock, processed_items) = cache.map_or_else(
            || self.utxo_delta(scripthash, HashMap::new(), 0),
            |(oldutxos, blockheight)| self.utxo_delta(scripthash, oldutxos, blockheight + 1),
        );

        // save updated utxo set to cache
        if let Some(lastblock) = lastblock {
            if had_cache || processed_items > MIN_HISTORY_ITEMS_TO_CACHE {
                self.store.cache_db.write(
                    vec![UtxoCacheRow::new(scripthash, &newutxos, &lastblock).to_row()],
                    DBFlush::Enable,
                );
            }
        }

        // format as Utxo objects
        newutxos
            .into_iter()
            .map(|(outpoint, (blockid, value))| Utxo {
                txid: outpoint.txid,
                vout: outpoint.vout,
                value,
                confirmed: Some(blockid),
            })
            .collect()
    }

    pub fn utxo_delta(
        &self,
        scripthash: &[u8],
        init_utxos: UtxoMap,
        start_height: usize,
    ) -> (UtxoMap, Option<Sha256dHash>, usize) {
        let _timer = self.start_timer("utxo_delta");
        let history_iter = self
            .history_iter_scan(scripthash, start_height)
            .map(TxHistoryRow::from_row)
            .filter_map(|history| {
                self.tx_confirming_block(&history.get_txid())
                    .map(|b| (history, b))
            });

        let mut utxos = init_utxos;
        let mut processed_items = 0;
        let mut lastblock = None;

        for (history, blockid) in history_iter {
            processed_items = processed_items + 1;
            lastblock = Some(blockid.hash);

            // TODO: make sure funding rows are processed before spending rows on the same height
            match history.key.txinfo {
                TxHistoryInfo::Funding(ref info) => {
                    utxos.insert(history.get_outpoint(), (blockid, info.value))
                }
                TxHistoryInfo::Spending(_) => utxos.remove(&history.get_outpoint()),
            };
        }

        (utxos, lastblock, processed_items)
    }

    pub fn stats(&self, scripthash: &[u8]) -> ScriptStats {
        let _timer = self.start_timer("stats");

        // get the last known stats and the blockhash they are updated for.
        // invalidates the cache if the block was orphaned.
        let cache: Option<(ScriptStats, usize)> = self
            .store
            .cache_db
            .get(&StatsCacheRow::key(scripthash))
            .map(|c| bincode::deserialize(&c).unwrap())
            .and_then(|(stats, blockhash)| {
                self.height_by_hash(&blockhash)
                    .map(|height| (stats, height))
            });

        // update stats with new transactions since
        let (newstats, lastblock) = cache.map_or_else(
            || self.stats_delta(scripthash, ScriptStats::default(), 0),
            |(oldstats, blockheight)| self.stats_delta(scripthash, oldstats, blockheight + 1),
        );

        // save updated stats to cache
        if let Some(lastblock) = lastblock {
            if newstats.funded_txo_count + newstats.spent_txo_count > MIN_HISTORY_ITEMS_TO_CACHE {
                self.store.cache_db.write(
                    vec![StatsCacheRow::new(scripthash, &newstats, &lastblock).to_row()],
                    DBFlush::Enable,
                );
            }
        }

        newstats
    }

    fn stats_delta(
        &self,
        scripthash: &[u8],
        init_stats: ScriptStats,
        start_height: usize,
    ) -> (ScriptStats, Option<Sha256dHash>) {
        let _timer = self.start_timer("stats_delta"); // TODO: measure also the number of txns processed.
        let history_iter = self
            .history_iter_scan(scripthash, start_height)
            .map(TxHistoryRow::from_row)
            .filter_map(|history| {
                self.tx_confirming_block(&history.get_txid())
                    .map(|blockid| (history, blockid))
            });

        let mut stats = init_stats;
        let mut seen_txids = HashSet::new();
        let mut lastblock = None;

        for (history, blockid) in history_iter {
            if lastblock != Some(blockid.hash) {
                seen_txids.clear();
            }

            if seen_txids.insert(history.get_txid()) {
                stats.tx_count += 1;
            }

            match history.key.txinfo {
                #[cfg(not(feature = "liquid"))]
                TxHistoryInfo::Funding(ref info) => {
                    stats.funded_txo_count += 1;
                    stats.funded_txo_sum += info.value;
                }
                #[cfg(feature = "liquid")]
                TxHistoryInfo::Funding(_) => {
                    stats.funded_txo_count += 1;
                }

                #[cfg(not(feature = "liquid"))]
                TxHistoryInfo::Spending(ref info) => {
                    stats.spent_txo_count += 1;
                    stats.spent_txo_sum += info.value;
                }
                #[cfg(feature = "liquid")]
                TxHistoryInfo::Spending(_) => {
                    stats.spent_txo_count += 1;
                }
            };

            lastblock = Some(blockid.hash);
        }

        (stats, lastblock)
    }

    fn header_by_hash(&self, hash: &Sha256dHash) -> Option<HeaderEntry> {
        self.store
            .indexed_headers
            .read()
            .unwrap()
            .header_by_blockhash(hash)
            .cloned()
    }

    // Get the height of a blockhash, only if its part of the best chain
    fn height_by_hash(&self, hash: &Sha256dHash) -> Option<usize> {
        self.store
            .indexed_headers
            .read()
            .unwrap()
            .header_by_blockhash(hash)
            .map(|header| header.height())
    }

    pub fn header_by_height(&self, height: usize) -> Option<HeaderEntry> {
        self.store
            .indexed_headers
            .read()
            .unwrap()
            .header_by_height(height)
            .cloned()
    }

    pub fn blockid_by_height(&self, height: usize) -> Option<BlockId> {
        self.store
            .indexed_headers
            .read()
            .unwrap()
            .header_by_height(height)
            .map(BlockId::from)
    }

    pub fn best_height(&self) -> usize {
        self.store.indexed_headers.read().unwrap().len() - 1
    }

    pub fn best_hash(&self) -> Sha256dHash {
        self.store.indexed_headers.read().unwrap().tip().clone()
    }

    pub fn best_header(&self) -> HeaderEntry {
        let headers = self.store.indexed_headers.read().unwrap();
        headers
            .header_by_blockhash(headers.tip())
            .expect("missing chain tip")
            .clone()
    }

    // TODO: can we pass txids as a "generic iterable"?
    // TODO: should also use a custom ThreadPoolBuilder?
    pub fn lookup_txns(&self, txids: &Vec<Sha256dHash>) -> Result<Vec<Transaction>> {
        let _timer = self.start_timer("lookup_txns");
        txids
            .par_iter()
            .map(|txid| self.lookup_txn(txid).chain_err(|| "missing tx"))
            .collect::<Result<Vec<Transaction>>>()
    }

    pub fn lookup_txn(&self, txid: &Sha256dHash) -> Option<Transaction> {
        let _timer = self.start_timer("lookup_txn");
        self.lookup_raw_txn(txid).map(|rawtx| {
            let txn: Transaction = deserialize(&rawtx).expect("failed to parse Transaction");
            assert_eq!(*txid, txn.txid());
            txn
        })
    }

    pub fn lookup_raw_txn(&self, txid: &Sha256dHash) -> Option<Bytes> {
        let _timer = self.start_timer("lookup_raw_txn");
        self.store.txstore_db.get(&TxRow::key(&txid[..]))
    }

    pub fn lookup_txo(&self, outpoint: &OutPoint) -> Option<TxOut> {
        let _timer = self.start_timer("lookup_txo");
        lookup_txo(&self.store.txstore_db, outpoint)
    }

    pub fn lookup_txos(&self, outpoints: &BTreeSet<OutPoint>) -> HashMap<OutPoint, TxOut> {
        let _timer = self.start_timer("lookup_txos");
        lookup_txos(&self.store.txstore_db, outpoints, false)
    }

    pub fn lookup_avail_txos(&self, outpoints: &BTreeSet<OutPoint>) -> HashMap<OutPoint, TxOut> {
        let _timer = self.start_timer("lookup_available_txos");
        lookup_txos(&self.store.txstore_db, outpoints, true)
    }

    pub fn lookup_spend(&self, outpoint: &OutPoint) -> Option<SpendingInput> {
        let _timer = self.start_timer("lookup_spend");
        self.store
            .history_db
            .iter_scan(&TxEdgeRow::filter(&outpoint))
            .map(TxEdgeRow::from_row)
            .find_map(|edge| {
                let txid = parse_hash(&edge.key.spending_txid);
                self.tx_confirming_block(&txid).map(|b| SpendingInput {
                    txid,
                    vin: edge.key.spending_vin as u32,
                    confirmed: Some(b),
                })
            })
    }
    pub fn tx_confirming_block(&self, txid: &Sha256dHash) -> Option<BlockId> {
        let _timer = self.start_timer("tx_confirming_block");
        let headers = self.store.indexed_headers.read().unwrap();
        self.store
            .txstore_db
            .iter_scan(&TxConfRow::filter(&txid[..]))
            .map(TxConfRow::from_row)
            // header_by_blockhash only returns blocks that are part of the best chain,
            // or None for orphaned blocks.
            .filter_map(|conf| headers.header_by_blockhash(&parse_hash(&conf.key.blockhash)))
            .nth(0)
            .map(BlockId::from)
    }

    pub fn get_block_status(&self, hash: &Sha256dHash) -> BlockStatus {
        // TODO differentiate orphaned and non-existing blocks? telling them apart requires
        // an additional db read.

        let headers = self.store.indexed_headers.read().unwrap();

        // header_by_blockhash only returns blocks that are part of the best chain,
        // or None for orphaned blocks.
        headers.header_by_blockhash(hash).map_or_else(
            || BlockStatus::orphaned(),
            |header| {
                BlockStatus::confirmed(
                    header.height(),
                    headers
                        .header_by_height(header.height() + 1)
                        .map(|h| h.hash().clone()),
                )
            },
        )
    }
}

fn load_blockhashes(db: &DB, prefix: &[u8]) -> HashSet<Sha256dHash> {
    db.iter_scan(prefix)
        .map(BlockRow::from_row)
        .map(|r| deserialize(&r.key.hash).expect("failed to parse Sha256dHash"))
        .collect()
}

fn load_blockheaders(db: &DB) -> HashMap<Sha256dHash, BlockHeader> {
    db.iter_scan(&BlockRow::header_filter())
        .map(BlockRow::from_row)
        .map(|r| {
            let key: Sha256dHash = deserialize(&r.key.hash).expect("failed to parse Sha256dHash");
            let value: BlockHeader = deserialize(&r.value).expect("failed to parse BlockHeader");
            (key, value)
        })
        .collect()
}

fn add_blocks(block_entries: &[BlockEntry]) -> Vec<DBRow> {
    // persist individual transactions:
    //      T{txid} → {rawtx}
    //      C{txid}{blockhash}{height} →
    //      O{txid}{index} → {txout}
    // persist block headers', block txids' and metadata rows:
    //      B{blockhash} → {header}
    //      X{blockhash} → {txid1}...{txidN}
    //      M{blockhash} → {tx_count}{size}{weight}
    block_entries
        .par_iter() // serialization is CPU-intensive
        .map(|b| {
            let mut rows = vec![];
            let blockhash = b.entry.hash().to_bytes();
            let txids: Vec<Sha256dHash> = b.block.txdata.iter().map(|tx| tx.txid()).collect();
            for tx in &b.block.txdata {
                add_transaction(tx, blockhash, &mut rows);
            }
            rows.push(BlockRow::new_header(&b).to_row());
            rows.push(BlockRow::new_txids(blockhash, &txids).to_row());
            rows.push(BlockRow::new_meta(blockhash, &BlockMeta::from(b)).to_row());
            rows.push(BlockRow::new_done(blockhash).to_row()); // mark block as "added"
            rows
        })
        .flatten()
        .collect()
}

fn add_transaction(tx: &Transaction, blockhash: FullHash, rows: &mut Vec<DBRow>) {
    rows.push(TxRow::new(tx).to_row());
    rows.push(TxConfRow::new(tx, blockhash).to_row());

    let txid = tx.txid().into_bytes();
    for (txo_index, txo) in tx.output.iter().enumerate() {
        if !txo.script_pubkey.is_provably_unspendable() {
            rows.push(TxOutRow::new(&txid, txo_index, txo).to_row());
        }
    }
}

fn get_previous_txos(block_entries: &[BlockEntry]) -> BTreeSet<OutPoint> {
    block_entries
        .iter()
        .flat_map(|b| b.block.txdata.iter())
        .flat_map(|tx| {
            tx.input
                .iter()
                .filter(|txin| has_prevout(txin))
                .map(|txin| txin.previous_output)
        })
        .collect()
}

fn lookup_txos(
    txstore_db: &DB,
    outpoints: &BTreeSet<OutPoint>,
    allow_missing: bool,
) -> HashMap<OutPoint, TxOut> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(16) // we need to saturate SSD IOPS
        .thread_name(|i| format!("lookup-txo-{}", i))
        .build()
        .unwrap();
    pool.install(|| {
        outpoints
            .par_iter()
            .filter_map(|outpoint| {
                lookup_txo(&txstore_db, &outpoint)
                    .or_else(|| {
                        if !allow_missing {
                            panic!("missing txo {} in {:?}", outpoint, txstore_db);
                        }
                        None
                    })
                    .map(|txo| (*outpoint, txo))
            })
            .collect()
    })
}

fn lookup_txo(txstore_db: &DB, outpoint: &OutPoint) -> Option<TxOut> {
    txstore_db
        .get(&TxOutRow::key(&outpoint))
        .map(|val| deserialize(&val).expect("failed to parse TxOut"))
}

fn index_blocks(
    block_entries: &[BlockEntry],
    previous_txos_map: &HashMap<OutPoint, TxOut>,
) -> Vec<DBRow> {
    block_entries
        .par_iter() // serialization is CPU-intensive
        .map(|b| {
            let mut rows = vec![];
            for tx in &b.block.txdata {
                let height = b.entry.height() as u32;
                index_transaction(tx, height, previous_txos_map, &mut rows);
            }
            rows.push(BlockRow::new_done(b.entry.hash().to_bytes()).to_row()); // mark block as "indexed"
            rows
        })
        .flatten()
        .collect()
}

// TODO: return an iterator?
fn index_transaction(
    tx: &Transaction,
    confirmed_height: u32,
    previous_txos_map: &HashMap<OutPoint, TxOut>,
    rows: &mut Vec<DBRow>,
) {
    // persist history index:
    //      H{funding-scripthash}{funding-height}F{funding-txid:vout} → ""
    //      H{funding-scripthash}{spending-height}S{spending-txid:vin}{funding-txid:vout} → ""
    // persist "edges" for fast is-this-TXO-spent check
    //      S{funding-txid:vout}{spending-txid:vin} → ""
    let txid = tx.txid().into_bytes();
    for (txo_index, txo) in tx.output.iter().enumerate() {
        if is_spendable(txo) {
            let history = TxHistoryRow::new(
                &txo.script_pubkey,
                confirmed_height,
                TxHistoryInfo::Funding(FundingInfo {
                    txid,
                    vout: txo_index as u16,
                    value: txo.value,
                }),
            );
            rows.push(history.to_row())
        }
    }
    for (txi_index, txi) in tx.input.iter().enumerate() {
        if !has_prevout(txi) {
            continue;
        }
        let prev_txo = previous_txos_map
            .get(&txi.previous_output)
            .expect(&format!("missing previous txo {}", txi.previous_output));

        let history = TxHistoryRow::new(
            &prev_txo.script_pubkey,
            confirmed_height,
            TxHistoryInfo::Spending(SpendingInfo {
                txid,
                vin: txi_index as u16,
                prev_txid: txi.previous_output.txid.into_bytes(),
                prev_vout: txi.previous_output.vout as u16,
                value: prev_txo.value,
            }),
        );
        rows.push(history.to_row());

        let edge = TxEdgeRow::new(
            txi.previous_output.txid.into_bytes(),
            txi.previous_output.vout as u16,
            txid,
            txi_index as u16,
        );
        rows.push(edge.to_row());
    }
}

// TODO: replace by a separate opaque type (similar to Sha256dHash, but without the "double")
pub type FullHash = [u8; 32]; // serialized SHA256 result

pub fn compute_script_hash(script: &Script) -> FullHash {
    let mut hash = FullHash::default();
    let mut sha2 = Sha256::new();
    sha2.input(script.as_bytes());
    sha2.result(&mut hash);
    hash
}

pub fn parse_hash(hash: &FullHash) -> Sha256dHash {
    deserialize(hash).expect("failed to parse Sha256dHash")
}

#[derive(Serialize, Deserialize)]
struct TxRowKey {
    code: u8,
    txid: FullHash,
}

struct TxRow {
    key: TxRowKey,
    value: Bytes, // raw transaction
}

impl TxRow {
    fn new(txn: &Transaction) -> TxRow {
        let txid = txn.txid().into_bytes();
        TxRow {
            key: TxRowKey { code: b'T', txid },
            value: serialize(txn),
        }
    }

    fn key(prefix: &[u8]) -> Bytes {
        [b"T", prefix].concat()
    }

    fn to_row(self) -> DBRow {
        let TxRow { key, value } = self;
        DBRow {
            key: bincode::serialize(&key).unwrap(),
            value,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct TxConfKey {
    code: u8,
    txid: FullHash,
    blockhash: FullHash,
}

struct TxConfRow {
    key: TxConfKey,
}

impl TxConfRow {
    fn new(txn: &Transaction, blockhash: FullHash) -> TxConfRow {
        let txid = txn.txid().into_bytes();
        TxConfRow {
            key: TxConfKey {
                code: b'C',
                txid,
                blockhash,
            },
        }
    }

    fn filter(prefix: &[u8]) -> Bytes {
        [b"C", prefix].concat()
    }

    fn to_row(self) -> DBRow {
        DBRow {
            key: bincode::serialize(&self.key).unwrap(),
            value: vec![],
        }
    }

    fn from_row(row: DBRow) -> Self {
        TxConfRow {
            key: bincode::deserialize(&row.key).expect("failed to parse TxConfKey"),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct TxOutKey {
    code: u8,
    txid: FullHash,
    vout: u16,
}

struct TxOutRow {
    key: TxOutKey,
    value: Bytes, // serialized output
}

impl TxOutRow {
    fn new(txid: &FullHash, vout: usize, txout: &TxOut) -> TxOutRow {
        TxOutRow {
            key: TxOutKey {
                code: b'O',
                txid: *txid,
                vout: vout as u16,
            },
            value: serialize(txout),
        }
    }
    fn key(outpoint: &OutPoint) -> Bytes {
        bincode::serialize(&TxOutKey {
            code: b'O',
            txid: outpoint.txid.to_bytes(),
            vout: outpoint.vout as u16,
        })
        .unwrap()
    }

    fn to_row(self) -> DBRow {
        DBRow {
            key: bincode::serialize(&self.key).unwrap(),
            value: self.value,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct BlockKey {
    code: u8,
    hash: FullHash,
}

struct BlockRow {
    key: BlockKey,
    value: Bytes, // serialized output
}

impl BlockRow {
    fn new_header(block_entry: &BlockEntry) -> BlockRow {
        BlockRow {
            key: BlockKey {
                code: b'B',
                hash: block_entry.entry.hash().to_bytes(),
            },
            value: serialize(&block_entry.block.header),
        }
    }

    fn new_txids(hash: FullHash, txids: &[Sha256dHash]) -> BlockRow {
        BlockRow {
            key: BlockKey { code: b'X', hash },
            value: bincode::serialize(txids).unwrap(),
        }
    }

    fn new_meta(hash: FullHash, meta: &BlockMeta) -> BlockRow {
        BlockRow {
            key: BlockKey { code: b'M', hash },
            value: bincode::serialize(meta).unwrap(),
        }
    }

    fn new_done(hash: FullHash) -> BlockRow {
        BlockRow {
            key: BlockKey { code: b'D', hash },
            value: vec![],
        }
    }

    fn header_filter() -> Bytes {
        b"B".to_vec()
    }

    fn txids_key(hash: FullHash) -> Bytes {
        [b"X", &hash[..]].concat()
    }

    fn meta_key(hash: FullHash) -> Bytes {
        [b"M", &hash[..]].concat()
    }

    fn done_filter() -> Bytes {
        b"D".to_vec()
    }

    fn to_row(self) -> DBRow {
        DBRow {
            key: bincode::serialize(&self.key).unwrap(),
            value: self.value,
        }
    }

    fn from_row(row: DBRow) -> Self {
        BlockRow {
            key: bincode::deserialize(&row.key).unwrap(),
            value: row.value,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct FundingInfo {
    pub txid: FullHash, // funding transaction
    pub vout: u16,
    pub value: Value,
}

#[derive(Serialize, Deserialize)]
pub struct SpendingInfo {
    pub txid: FullHash, // spending transaction
    pub vin: u16,
    pub prev_txid: FullHash, // funding transaction
    pub prev_vout: u16,
    pub value: Value,
}

#[derive(Serialize, Deserialize)]
pub enum TxHistoryInfo {
    Funding(FundingInfo),
    Spending(SpendingInfo),
}

#[derive(Serialize, Deserialize)]
struct TxHistoryKey {
    code: u8,
    scripthash: FullHash,
    confirmed_height: u32, // MUST be serialized as big-endian (for correct scans).
    txinfo: TxHistoryInfo,
}

struct TxHistoryRow {
    key: TxHistoryKey,
}

impl TxHistoryRow {
    fn new(script: &Script, confirmed_height: u32, txinfo: TxHistoryInfo) -> Self {
        let key = TxHistoryKey {
            code: b'H',
            scripthash: compute_script_hash(&script),
            confirmed_height,
            txinfo,
        };
        TxHistoryRow { key }
    }

    fn filter(scripthash_prefix: &[u8]) -> Bytes {
        [b"H", scripthash_prefix].concat()
    }

    fn prefix_end(scripthash: &[u8]) -> Bytes {
        bincode::serialize(&(b'H', full_hash(&scripthash[..]), std::u32::MAX)).unwrap()
    }

    fn prefix_height(scripthash: &[u8], height: u32) -> Bytes {
        bincode::config()
            .big_endian()
            .serialize(&(b'H', full_hash(&scripthash[..]), height))
            .unwrap()
    }

    fn to_row(self) -> DBRow {
        DBRow {
            key: bincode::config().big_endian().serialize(&self.key).unwrap(),
            value: vec![],
        }
    }

    fn from_row(row: DBRow) -> Self {
        let key = bincode::config()
            .big_endian()
            .deserialize(&row.key)
            .expect("failed to deserialize TxHistoryKey");
        TxHistoryRow { key }
    }

    fn get_txid(&self) -> Sha256dHash {
        match self.key.txinfo {
            TxHistoryInfo::Funding(ref info) => parse_hash(&info.txid),
            TxHistoryInfo::Spending(ref info) => parse_hash(&info.txid),
        }
    }

    // for funding rows, returns the funded output.
    // for spending rows, returns the spent previous output.
    fn get_outpoint(&self) -> OutPoint {
        match self.key.txinfo {
            TxHistoryInfo::Funding(ref info) => OutPoint {
                txid: parse_hash(&info.txid),
                vout: info.vout as u32,
            },
            TxHistoryInfo::Spending(ref info) => OutPoint {
                txid: parse_hash(&info.prev_txid),
                vout: info.prev_vout as u32,
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
struct TxEdgeKey {
    code: u8,
    funding_txid: FullHash,
    funding_vout: u16,
    spending_txid: FullHash,
    spending_vin: u16,
}

struct TxEdgeRow {
    key: TxEdgeKey,
}

impl TxEdgeRow {
    fn new(
        funding_txid: FullHash,
        funding_vout: u16,
        spending_txid: FullHash,
        spending_vin: u16,
    ) -> Self {
        let key = TxEdgeKey {
            code: b'S',
            funding_txid,
            funding_vout,
            spending_txid,
            spending_vin,
        };
        TxEdgeRow { key }
    }

    fn filter(outpoint: &OutPoint) -> Bytes {
        // TODO build key without using bincode? [ b"S", &outpoint.txid[..], outpoint.vout?? ].concat()
        bincode::serialize(&(b'S', full_hash(&outpoint.txid[..]), outpoint.vout as u16)).unwrap()
    }

    fn to_row(self) -> DBRow {
        DBRow {
            key: bincode::serialize(&self.key).unwrap(),
            value: vec![],
        }
    }

    fn from_row(row: DBRow) -> Self {
        TxEdgeRow {
            key: bincode::deserialize(&row.key).expect("failed to deserialize TxEdgeKey"),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct ScriptCacheKey {
    code: u8,
    scripthash: FullHash,
}

struct StatsCacheRow {
    key: ScriptCacheKey,
    value: Bytes,
}

impl StatsCacheRow {
    fn new(scripthash: &[u8], stats: &ScriptStats, blockhash: &Sha256dHash) -> Self {
        StatsCacheRow {
            key: ScriptCacheKey {
                code: b'A',
                scripthash: full_hash(scripthash),
            },
            value: bincode::serialize(&(stats, blockhash)).unwrap(),
        }
    }

    pub fn key(scripthash: &[u8]) -> Bytes {
        [b"A", scripthash].concat()
    }

    fn to_row(self) -> DBRow {
        DBRow {
            key: bincode::serialize(&self.key).unwrap(),
            value: self.value,
        }
    }
}

type CachedUtxoMap = HashMap<(Sha256dHash, u32), (u32, Value)>; // (txid,vout) => (block_height,output_value)

struct UtxoCacheRow {
    key: ScriptCacheKey,
    value: Bytes,
}

impl UtxoCacheRow {
    fn new(scripthash: &[u8], utxos: &UtxoMap, blockhash: &Sha256dHash) -> Self {
        let utxos_cache = UtxoCacheRow::to_utxo_cache(utxos);

        UtxoCacheRow {
            key: ScriptCacheKey {
                code: b'U',
                scripthash: full_hash(scripthash),
            },
            value: bincode::serialize(&(utxos_cache, blockhash)).unwrap(),
        }
    }

    pub fn key(scripthash: &[u8]) -> Bytes {
        [b"U", scripthash].concat()
    }

    // keep utxo cache with just the block height (the hash/timestamp are read later from the headers to reconstruct BlockId)
    // and use a (txid,vout) tuple instead of OutPoints (they don't play nicely with bincode serialization)
    pub fn to_utxo_cache(utxos: &UtxoMap) -> CachedUtxoMap {
        utxos
            .iter()
            .map(|(outpoint, (blockid, value))| {
                (
                    (outpoint.txid, outpoint.vout),
                    (blockid.height as u32, *value),
                )
            })
            .collect()
    }

    pub fn to_utxo_map(utxos_cache: CachedUtxoMap, chain: &ChainQuery) -> UtxoMap {
        utxos_cache
            .into_iter()
            .map(|((txid, vout), (height, value))| {
                let outpoint = OutPoint { txid, vout };
                let blockid = chain
                    .blockid_by_height(height as usize)
                    .expect("missing blockheader for valid utxo cache entry");
                (outpoint, (blockid, value))
            })
            .collect()
    }

    fn to_row(self) -> DBRow {
        DBRow {
            key: bincode::serialize(&self.key).unwrap(),
            value: self.value,
        }
    }
}
