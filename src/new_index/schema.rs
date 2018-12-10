use bincode;
use bitcoin::blockdata::block::BlockHeader;
use bitcoin::blockdata::script::Script;
use bitcoin::blockdata::transaction::{OutPoint, Transaction, TxOut};
use bitcoin::consensus::encode::{deserialize, serialize};
use bitcoin::util::hash::Sha256dHash;
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use itertools::Itertools;
use rayon::prelude::*;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::RwLock;

// use metrics::{Counter, Gauge, HistogramOpts, HistogramTimer, HistogramVec, MetricOpts, Metrics};
// use signal::Waiter;
use crate::daemon::Daemon;
use crate::errors::*;
use crate::util::{Bytes, HeaderList};

use crate::new_index::db::{DBRow, DB};
use crate::new_index::fetch::{start_fetcher, BlockEntry, FetchFrom};

pub struct Store {
    txstore_db: DB,
    history_db: DB,
}

impl Store {
    pub fn open(path: &Path) -> Self {
        Store {
            txstore_db: DB::open(&path.join("txstore")),
            history_db: DB::open(&path.join("history")),
        }
    }

    fn indexed_headers(&self) -> HeaderList {
        // TODO: read (and verify) from DB
        HeaderList::empty()
    }
}

pub struct Indexer<'a> {
    // TODO: should be column families
    store: &'a Store,
    indexed_headers: RwLock<HeaderList>,
    added_blockhashes: HashSet<Sha256dHash>,
}

pub struct Query<'a> {
    indexer: &'a Indexer<'a>,
}

#[derive(Debug)]
pub struct BlockId(usize, Sha256dHash);

#[derive(Debug)]
pub struct Utxo {
    txid: Sha256dHash,
    vout: u32,
    txout: TxOut,
    confirmed: BlockId,
}

// TODO: &[Block] should be an iterator / a queue.
impl<'a> Indexer<'a> {
    pub fn open(store: &'a Store) -> Self {
        Indexer {
            store,
            indexed_headers: RwLock::new(store.indexed_headers()), // TODO: sync from db
            added_blockhashes: HashSet::new(),
        }
    }

    pub fn update(&mut self, daemon: &Daemon, from: FetchFrom) -> Result<()> {
        let daemon = daemon.reconnect()?;
        let tip = daemon.getbestblockhash()?;

        let new_headers = {
            let headers = self.indexed_headers.read().unwrap();
            headers.order(daemon.get_new_headers(&headers, &tip)?)
        };

        info!("adding transactions from {} blocks", new_headers.len());
        start_fetcher(from, &daemon, &new_headers)?.map(|blocks| self.add(&blocks));

        info!("compacting txns DB");
        self.store.txstore_db.compact_all();

        info!("indexing history from {} blocks", new_headers.len());
        start_fetcher(from, &daemon, &new_headers)?.map(|blocks| self.index(&blocks));

        info!("compacting history DB");
        self.store.history_db.compact_all();

        let mut headers = self.indexed_headers.write().unwrap();
        headers.apply(new_headers);
        assert_eq!(tip, *headers.tip());
        Ok(())
    }

    pub fn query(&self) -> Query {
        Query { indexer: self }
    }

    fn add(&mut self, blocks: &[BlockEntry]) {
        // TODO: skip orphaned blocks?
        let rows = add_blocks(blocks);
        self.store.txstore_db.write(rows);

        self.added_blockhashes
            .extend(blocks.into_iter().map(|b| b.entry.hash()));
    }

    fn index(&mut self, blocks: &[BlockEntry]) {
        debug!("looking up TXOs from {} blocks", blocks.len());
        let previous_txos_map = self.query().lookup_txos(&get_previous_txos(blocks));
        debug!("looked up {} TXOs", previous_txos_map.len());
        for b in blocks {
            let blockhash = b.entry.hash();
            // TODO: replace by lookup into txstore_db?
            if !self.added_blockhashes.contains(&blockhash) {
                panic!("cannot index block {} (missing from store)", blockhash);
            }
        }
        let rows = index_blocks(blocks, &previous_txos_map);
        debug!("indexed {} history rows", rows.len());
        self.store.history_db.write(rows);
        debug!("written to DB");
    }
}

impl<'a> Query<'a> {
    pub fn get_block_header(&self, hash: &Sha256dHash) -> Option<BlockHeader> {
        self.indexer
            .store
            .txstore_db
            .get(&BlockRow::header_key(hash.to_bytes()))
            .map(|val| deserialize(&val).expect("failed to parse BlockHeader"))
    }

    pub fn get_block_txids(&self, hash: &Sha256dHash) -> Option<Vec<Sha256dHash>> {
        self.indexer
            .store
            .txstore_db
            .get(&BlockRow::txids_key(hash.to_bytes()))
            .map(|val| bincode::deserialize(&val).expect("failed to parse BlockHeader"))
    }

    pub fn history(&self, script: &Script) -> HashMap<Sha256dHash, (Transaction, BlockId)> {
        let scripthash = compute_script_hash(script.as_bytes());
        let rows = self
            .indexer
            .store
            .history_db
            .scan(&TxHistoryRow::filter(&scripthash[..]));
        let mut txnsconf = rows
            .into_iter()
            .map(|row| TxHistoryRow::from_row(row).get_txid())
            .dedup()
            .filter_map(|txid| self.tx_confirming_block(&txid).map(|b| (txid, b)))
            .collect::<HashMap<Sha256dHash, BlockId>>();

        let txids = txnsconf.keys().cloned().collect();
        let txns = self.lookup_txns(&txids);

        txns.into_iter()
            .map(|(txid, txn)| (txid, (txn, txnsconf.remove(&txid).unwrap())))
            .collect()
    }

    pub fn utxo(&self, script: &Script) -> Vec<Utxo> {
        let scripthash = compute_script_hash(script.as_bytes());
        let mut utxosconf = self
            .indexer
            .store
            .history_db
            .scan(&TxHistoryRow::filter(&scripthash[..]))
            .into_iter()
            .map(TxHistoryRow::from_row)
            .filter_map(|history| {
                self.tx_confirming_block(&history.get_txid())
                    .map(|b| (history, b))
            })
            .fold(HashMap::new(), |mut utxos, (history, block)| {
                match history.key.txinfo {
                    TxHistoryInfo::Funding(..) => utxos.insert(history.get_outpoint(), block),
                    TxHistoryInfo::Spending(..) => utxos.remove(&history.get_outpoint()),
                };
                // TODO: make sure funding rows are processed before spending rows on the same height
                utxos
            });

        let outpoints = utxosconf.keys().cloned().collect();
        let txos = self.lookup_txos(&outpoints);

        txos.into_iter()
            .map(|(outpoint, txo)| Utxo {
                txid: outpoint.txid,
                vout: outpoint.vout,
                txout: txo,
                confirmed: utxosconf.remove(&outpoint).unwrap(),
            })
            .collect()
    }

    // TODO: can we pass txids as a "generic iterable"?
    fn lookup_txns(&self, txids: &BTreeSet<Sha256dHash>) -> HashMap<Sha256dHash, Transaction> {
        txids
            .par_iter()
            .map(|txid| {
                let rawtx = self
                    .indexer
                    .store
                    .txstore_db
                    .get(&TxRow::key(&txid[..]))
                    .expect(&format!("missing txid {} in txns db", txid));
                let txn: Transaction = deserialize(&rawtx).expect("failed to parse Transaction");
                assert_eq!(*txid, txn.txid());
                (*txid, txn)
            })
            .collect()
    }

    fn lookup_txos(&self, outpoints: &BTreeSet<OutPoint>) -> HashMap<OutPoint, TxOut> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(16) // we need to saturate SSD IOPS
            .thread_name(|i| format!("lookup-txo-{}", i))
            .build()
            .unwrap();
        pool.install(|| {
            outpoints
                .par_iter()
                .map(|outpoint| {
                    let txo = self.lookup_txo(&outpoint).expect("missing txo in txns db");
                    (*outpoint, txo)
                })
                .collect()
        })
    }

    fn lookup_txo(&self, outpoint: &OutPoint) -> Option<TxOut> {
        self.indexer
            .store
            .txstore_db
            .get(&TxOutRow::key(&outpoint))
            .map(|val| deserialize(&val).expect("failed to parse TxOut"))
    }

    fn tx_confirming_block(&self, txid: &Sha256dHash) -> Option<BlockId> {
        self.indexer
            .store
            .txstore_db
            .scan(&TxConfRow::filter(&txid[..]))
            .into_iter()
            .map(TxConfRow::from_row)
            .find_map(|conf| {
                let headers = self.indexer.indexed_headers.read().unwrap();
                headers
                    .header_by_blockhash(&parse_hash(&conf.key.blockhash))
                    .map(|h| BlockId(h.height(), *h.hash()))
            })
    }
}

type FullHash = [u8; 32]; // serialized SHA256 result

fn parse_hash(hash: &FullHash) -> Sha256dHash {
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

    fn from_row(row: DBRow) -> Self {
        let DBRow { key, value } = row;
        TxRow {
            key: bincode::deserialize(&key).expect("failed to parse TxRowKey"),
            value,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct TxConfKey {
    code: u8,
    txid: FullHash,
    blockheight: u32,
    blockhash: FullHash,
}

struct TxConfRow {
    key: TxConfKey,
}

impl TxConfRow {
    fn new(txn: &Transaction, blockheight: u32, blockhash: FullHash) -> TxConfRow {
        let txid = txn.txid().into_bytes();
        TxConfRow {
            key: TxConfKey {
                code: b'C',
                txid,
                blockheight,
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
    fn new_header(hash: FullHash, header: &BlockHeader) -> BlockRow {
        BlockRow {
            key: BlockKey { code: b'B', hash },
            value: serialize(header),
        }
    }

    fn new_txids(hash: FullHash, txids: &[Sha256dHash]) -> BlockRow {
        BlockRow {
            key: BlockKey { code: b'X', hash },
            value: bincode::serialize(txids).unwrap(),
        }
    }

    fn header_key(hash: FullHash) -> Bytes {
        [b"B", &hash[..]].concat()
    }

    fn txids_key(hash: FullHash) -> Bytes {
        [b"X", &hash[..]].concat()
    }

    fn to_row(self) -> DBRow {
        DBRow {
            key: bincode::serialize(&self.key).unwrap(),
            value: self.value,
        }
    }
}

fn add_blocks(block_entries: &[BlockEntry]) -> Vec<DBRow> {
    // persist individual transactions:
    //      T{txid} → {rawtx}
    //      C{txid}{blockhash}{height} →
    //      O{txid}{index} → {txout}
    // persist block headers' and txids' rows:
    //      B{blockhash} → {header}
    //      X{blockhash} → {txid1}...{txidN}
    block_entries
        .par_iter() // serialization is CPU-intensive
        .map(|b| {
            let mut rows = vec![];
            let blockheight = b.entry.height() as u32;
            let blockhash = b.entry.hash().to_bytes();
            let txids: Vec<Sha256dHash> = b.block.txdata.iter().map(|tx| tx.txid()).collect();

            rows.push(BlockRow::new_header(blockhash, &b.block.header).to_row());
            rows.push(BlockRow::new_txids(blockhash, &txids).to_row());

            for tx in &b.block.txdata {
                add_transaction(tx, blockheight, blockhash, &mut rows);
            }
            rows
        })
        .flatten()
        .collect()
}

fn add_transaction(tx: &Transaction, blockheight: u32, blockhash: FullHash, rows: &mut Vec<DBRow>) {
    rows.push(TxRow::new(tx).to_row());
    rows.push(TxConfRow::new(tx, blockheight, blockhash).to_row());

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
        .flat_map(|b| {
            b.block.txdata.iter().flat_map(|tx| {
                tx.input.iter().filter_map(|txin| {
                    if txin.previous_output.is_null() {
                        None
                    } else {
                        Some(txin.previous_output)
                    }
                })
            })
        })
        .collect()
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
        if !txo.script_pubkey.is_provably_unspendable() {
            let history = TxHistoryRow::new(
                &txo.script_pubkey,
                confirmed_height,
                TxHistoryInfo::Funding(txid, txo_index as u16),
            );
            rows.push(history.to_row())
        }
    }
    for (txi_index, txi) in tx.input.iter().enumerate() {
        if txi.previous_output.is_null() {
            continue;
        }
        let prev_txo = previous_txos_map
            .get(&txi.previous_output)
            .expect(&format!("missing previous txo {}", txi.previous_output));

        let history = TxHistoryRow::new(
            &prev_txo.script_pubkey,
            confirmed_height,
            TxHistoryInfo::Spending(
                txid,
                txi_index as u16,
                txi.previous_output.txid.into_bytes(),
                txi.previous_output.vout as u16,
            ),
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

fn compute_script_hash(script: &[u8]) -> FullHash {
    let mut hash = FullHash::default();
    let mut sha2 = Sha256::new();
    sha2.input(script);
    sha2.result(&mut hash);
    hash
}

#[derive(Serialize, Deserialize)]
enum TxHistoryInfo {
    Funding(FullHash, u16),                 // funding txid/vout
    Spending(FullHash, u16, FullHash, u16), // spending txid/vin and previous funding txid/vout
}

#[derive(Serialize, Deserialize)]
struct TxHistoryKey {
    code: u8,
    scripthash: FullHash,
    confirmed_height: u32,
    txinfo: TxHistoryInfo,
}

struct TxHistoryRow {
    key: TxHistoryKey,
}

impl TxHistoryRow {
    fn new(script: &Script, confirmed_height: u32, txinfo: TxHistoryInfo) -> Self {
        let key = TxHistoryKey {
            code: b'H',
            scripthash: compute_script_hash(&script[..]),
            confirmed_height,
            txinfo,
        };
        TxHistoryRow { key }
    }

    fn filter(scripthash_prefix: &[u8]) -> Bytes {
        [b"H", scripthash_prefix].concat()
    }

    fn to_row(self) -> DBRow {
        DBRow {
            key: bincode::serialize(&self.key).unwrap(),
            value: vec![],
        }
    }

    fn from_row(row: DBRow) -> Self {
        let key = bincode::deserialize(&row.key).expect("failed to deserialize TxHistoryKey");
        TxHistoryRow { key }
    }

    fn get_txid(&self) -> Sha256dHash {
        match self.key.txinfo {
            TxHistoryInfo::Funding(txid, ..) => parse_hash(&txid),
            TxHistoryInfo::Spending(txid, ..) => parse_hash(&txid),
        }
    }

    // for funding rows, returns the funded output.
    // for spending rows, returns the spent previous output.
    fn get_outpoint(&self) -> OutPoint {
        match self.key.txinfo {
            TxHistoryInfo::Funding(txid, vout) => OutPoint {
                txid: parse_hash(&txid),
                vout: vout as u32,
            },
            TxHistoryInfo::Spending(_, _, prevtxid, prevout) => OutPoint {
                txid: parse_hash(&prevtxid),
                vout: prevout as u32,
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

// TODO: add Block metadata row (# of txs, size and weight)
