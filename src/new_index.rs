use bincode;
use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::blockdata::script::Script;
use bitcoin::blockdata::transaction::{Transaction, TxOut};
use bitcoin::consensus::encode::{deserialize, serialize, Decodable};
use bitcoin::util::hash::{BitcoinHash, Sha256dHash};
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use rayon::prelude::*;
use rocksdb;

struct DBRow {
    key: Vec<u8>,
    value: Vec<u8>,
}

// use crypto::digest::Digest;
// use crypto::sha2::Sha256;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{Cursor, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::thread;

// use daemon::Daemon;
// use metrics::{Counter, Gauge, HistogramOpts, HistogramTimer, HistogramVec, MetricOpts, Metrics};
// use signal::Waiter;
use crate::daemon::Daemon;
use crate::errors::*;
use crate::util::{spawn_thread, Bytes, HeaderEntry, HeaderList, SyncChannel};

// use config::Config;

pub struct Indexer {
    // TODO: should be column families
    txstore_db: rocksdb::DB,
    history_db: rocksdb::DB,

    added_blockhashes: HashSet<Sha256dHash>,
}

struct BlockEntry {
    block: Block,
    entry: HeaderEntry,
}

pub enum FetchFrom {
    BITCOIND,
    BLKFILES,
}

// TODO: &[Block] should be an iterator / a queue.
impl Indexer {
    pub fn open(path: &Path) -> Self {
        Indexer {
            txstore_db: db_open(&path.join("txstore")),
            history_db: db_open(&path.join("history")),
            added_blockhashes: HashSet::new(),
        }
    }

    pub fn update(
        &mut self,
        daemon: &Daemon,
        headers: HeaderList,
        fetch: FetchFrom,
    ) -> Result<HeaderList> {
        let daemon = daemon.reconnect()?;
        let tip = daemon.getbestblockhash()?;
        let new_headers = headers.order(daemon.get_new_headers(&headers, &tip)?);

        let fetcher = match fetch {
            FetchFrom::BITCOIND => bitcoind_fetcher,
            FetchFrom::BLKFILES => blkfiles_fetcher,
        };

        info!("adding transactions from {} blocks", new_headers.len());
        fetcher(&daemon, &new_headers)?.map(|blocks| self.add(&blocks));

        info!("compacting txns DB");
        self.txstore_db.compact_range(None, None);

        info!("indexing history from {} blocks", new_headers.len());
        fetcher(&daemon, &new_headers)?.map(|blocks| self.index(&blocks));

        info!("compacting history DB");
        self.history_db.compact_range(None, None);

        let mut headers = headers;
        headers.apply(new_headers);
        assert_eq!(tip, *headers.tip());
        Ok(headers)
    }

    pub fn get_block_header(&self, hash: &Sha256dHash) -> Option<BlockHeader> {
        db_get(&self.txstore_db, &BlockRow::header_key(hash.to_bytes()))
            .map(|val| deserialize(&val).expect("failed to parse BlockHeader"))
    }

    pub fn get_block_txids(&self, hash: &Sha256dHash) -> Option<Vec<Sha256dHash>> {
        db_get(&self.txstore_db, &BlockRow::txids_key(hash.to_bytes()))
            .map(|val| bincode::deserialize(&val).expect("failed to parse BlockHeader"))
    }

    pub fn history(&self, script: &Script) -> HashMap<Sha256dHash, Transaction> {
        let scripthash = compute_script_hash(script.as_bytes());
        let rows = db_scan(&self.history_db, &TxHistoryRow::filter(&scripthash[..]));
        let txids = rows
            .into_iter()
            .map(TxHistoryRow::from_row)
            .map(|history_row| match history_row.key.txinfo {
                TxHistoryInfo::Funding(txid, ..) => txid,
                TxHistoryInfo::Spending(txid, ..) => txid,
            })
            .map(|txid| deserialize(&txid).expect("failed to deserialize Sha256dHash txid"))
            // TODO: check tx confirmation status
            .collect();
        debug!("txids: {:?}", txids);
        self.lookup_txns(&txids)
    }

    fn add(&mut self, blocks: &[BlockEntry]) {
        // TODO: skip orphaned blocks?
        let rows = add_blocks(blocks);
        db_write(&self.txstore_db, rows);

        self.added_blockhashes
            .extend(blocks.into_iter().map(|b| b.entry.hash()));
    }

    fn index(&mut self, blocks: &[BlockEntry]) {
        let previous_txos_map = self.lookup_txos(&get_previous_txos(blocks));
        for b in blocks {
            let blockhash = b.entry.hash();
            // TODO: replace by lookup into txstore_db?
            if !self.added_blockhashes.contains(&blockhash) {
                panic!("cannot index block {} (missing from store)", blockhash);
            }
        }
        let rows = index_blocks(blocks, &previous_txos_map);
        db_write(&self.history_db, rows);
    }

    // TODO: can we pass txids as a "generic iterable"?
    fn lookup_txns(&self, txids: &BTreeSet<Sha256dHash>) -> HashMap<Sha256dHash, Transaction> {
        txids
            .par_iter()
            .map(|txid| {
                let rawtx = db_get(&self.txstore_db, &TxRow::key(&txid[..]))
                    .expect(&format!("missing txid {} in txns db", txid));
                let txn: Transaction = deserialize(&rawtx).expect("failed to parse Transaction");
                assert_eq!(*txid, txn.txid());
                (*txid, txn)
            })
            .collect()
    }

    fn lookup_txos(
        &self,
        txos: &BTreeSet<(Sha256dHash, usize)>,
    ) -> HashMap<(Sha256dHash, usize), TxOut> {
        txos.par_iter()
            .map(|(txid, vout)| {
                let txo = self
                    .lookup_txo(&txid, *vout)
                    .expect("missing txo in txns db");
                ((*txid, *vout), txo)
            })
            .collect()
    }

    fn lookup_txo(&self, txid: &Sha256dHash, vout: usize) -> Option<TxOut> {
        db_get(&self.txstore_db, &TxOutRow::key(txid, vout))
            .map(|val| deserialize(&val).expect("failed to parse TxOut"))
    }
}

struct Fetcher<T> {
    receiver: Receiver<T>,
    thread: thread::JoinHandle<()>,
}

impl<T> Fetcher<T> {
    fn from(receiver: Receiver<T>, thread: thread::JoinHandle<()>) -> Self {
        Fetcher { receiver, thread }
    }

    fn map<F>(self, mut func: F)
    where
        F: FnMut(T) -> (),
    {
        for item in self.receiver {
            func(item);
        }
        self.thread.join().expect("fetcher thread panicked")
    }
}

fn bitcoind_fetcher(
    daemon: &Daemon,
    new_headers: &[HeaderEntry],
) -> Result<Fetcher<Vec<BlockEntry>>> {
    new_headers.last().map(|tip| {
        info!("{:?} ({} new headers)", tip, new_headers.len());
    });
    let new_headers = new_headers.to_vec();
    let daemon = daemon.reconnect()?;
    let chan = SyncChannel::new(1);
    let sender = chan.sender();
    Ok(Fetcher::from(
        chan.into_receiver(),
        spawn_thread("bitcoind_fetcher", move || -> () {
            for entries in new_headers.chunks(100) {
                let blockhashes: Vec<Sha256dHash> = entries.iter().map(|e| *e.hash()).collect();
                let blocks = daemon
                    .getblocks(&blockhashes)
                    .expect("failed to get blocks from bitcoind");
                assert_eq!(blocks.len(), entries.len());
                let block_entries: Vec<BlockEntry> = blocks
                    .into_iter()
                    .zip(entries)
                    .map(|(block, entry)| BlockEntry {
                        block,
                        entry: entry.clone(), // TODO: remove this clone()
                    })
                    .collect();
                assert_eq!(block_entries.len(), entries.len());
                sender
                    .send(block_entries)
                    .expect("failed to send fetched blocks");
            }
        }),
    ))
}

fn blkfiles_fetcher(
    daemon: &Daemon,
    new_headers: &[HeaderEntry],
) -> Result<Fetcher<Vec<BlockEntry>>> {
    let magic = daemon.magic();
    let blk_files = daemon.list_blk_files()?;

    let chan = SyncChannel::new(1);
    let sender = chan.sender();

    let mut entry_map: HashMap<Sha256dHash, HeaderEntry> =
        new_headers.iter().map(|h| (*h.hash(), h.clone())).collect();

    let parser = blkfiles_parser(blkfiles_reader(blk_files), magic);
    Ok(Fetcher::from(
        chan.into_receiver(),
        spawn_thread("bitcoind_fetcher", move || -> () {
            parser.map(|blocks| {
                let block_entries: Vec<BlockEntry> = blocks
                    .into_iter()
                    .filter_map(|block| {
                        let blockhash = block.bitcoin_hash();
                        entry_map
                            .remove(&blockhash)
                            .map(|entry| BlockEntry { block, entry })
                            .or_else(|| {
                                debug!("unknown block {}", blockhash);
                                None
                            })
                    })
                    .collect();
                trace!("fetched {} blocks", block_entries.len());
                sender
                    .send(block_entries)
                    .expect("failed to send blocks entries from blk*.dat files");
            });
            if !entry_map.is_empty() {
                panic!(
                    "failed to index {} blocks from blk*.dat files",
                    entry_map.len()
                )
            }
        }),
    ))
}

fn blkfiles_reader(blk_files: Vec<PathBuf>) -> Fetcher<Vec<u8>> {
    let chan = SyncChannel::new(1);
    let sender = chan.sender();

    Fetcher::from(
        chan.into_receiver(),
        spawn_thread("blkfiles_reader", move || -> () {
            for path in blk_files {
                trace!("reading {:?}", path);
                let blob = fs::read(&path).expect(&format!("failed to read {:?}", path));
                sender
                    .send(blob)
                    .expect(&format!("failed to send {:?} contents", path));
            }
        }),
    )
}

fn blkfiles_parser(blobs: Fetcher<Vec<u8>>, magic: u32) -> Fetcher<Vec<Block>> {
    let chan = SyncChannel::new(1);
    let sender = chan.sender();

    Fetcher::from(
        chan.into_receiver(),
        spawn_thread("blkfiles_parser", move || -> () {
            blobs.map(|blob| {
                trace!("parsing {} bytes", blob.len());
                let blocks = parse_blocks(blob, magic).expect("failed to parse blk*.dat file");
                sender
                    .send(blocks)
                    .expect("failed to send blocks from blk*.dat file");
            });
        }),
    )
}

fn parse_blocks(blob: Vec<u8>, magic: u32) -> Result<Vec<Block>> {
    let mut cursor = Cursor::new(&blob);
    let mut blocks = vec![];
    let max_pos = blob.len() as u64;
    while cursor.position() < max_pos {
        match u32::consensus_decode(&mut cursor) {
            Ok(value) => {
                if magic != value {
                    cursor
                        .seek(SeekFrom::Current(-3))
                        .expect("failed to seek back");
                    continue;
                }
            }
            Err(_) => break, // EOF
        };
        let block_size = u32::consensus_decode(&mut cursor).chain_err(|| "no block size")?;
        let start = cursor.position() as usize;
        cursor
            .seek(SeekFrom::Current(block_size as i64))
            .chain_err(|| format!("seek {} failed", block_size))?;
        let end = cursor.position() as usize;

        let block: Block = deserialize(&blob[start..end])
            .chain_err(|| format!("failed to parse block at {}..{}", start, end))?;
        blocks.push(block);
    }
    Ok(blocks)
}

fn db_open(path: &Path) -> rocksdb::DB {
    debug!("opening DB at {:?}", path);
    let mut db_opts = rocksdb::Options::default();
    db_opts.create_if_missing(true);
    db_opts.set_max_open_files(-1); // TODO: make sure to `ulimit -n` this process correctly
    db_opts.set_compaction_style(rocksdb::DBCompactionStyle::Level);
    db_opts.set_compression_type(rocksdb::DBCompressionType::Snappy);
    db_opts.set_target_file_size_base(256 << 20);
    db_opts.set_write_buffer_size(256 << 20);
    db_opts.set_disable_auto_compactions(true); // for initial bulk load

    // db_opts.set_advise_random_on_open(???);
    db_opts.set_compaction_readahead_size(1 << 20);

    // let mut block_opts = rocksdb::BlockBasedOptions::default();
    // block_opts.set_block_size(???);

    rocksdb::DB::open(&db_opts, path).expect("failed to open RocksDB")
}

fn db_scan(db: &rocksdb::DB, prefix: &[u8]) -> Vec<DBRow> {
    let mode = rocksdb::IteratorMode::From(prefix, rocksdb::Direction::Forward);
    db.iterator(mode)
        .take_while(|(key, _)| key.starts_with(prefix))
        .map(|(k, v)| DBRow {
            key: k.into_vec(),
            value: v.into_vec(),
        })
        .collect()
}

fn db_write(db: &rocksdb::DB, mut rows: Vec<DBRow>) {
    trace!("writing {} rows to {:?}", rows.len(), db);
    rows.sort_unstable_by(|a, b| a.key.cmp(&b.key));
    let mut batch = rocksdb::WriteBatch::default();
    for row in rows {
        batch.put(&row.key, &row.value).unwrap();
    }
    let mut opts = rocksdb::WriteOptions::new();
    opts.set_sync(false);
    opts.disable_wal(true);
    db.write_opt(batch, &opts).unwrap();
}

fn db_get(db: &rocksdb::DB, key: &[u8]) -> Option<Bytes> {
    db.get(key).unwrap().map(|v| v.to_vec())
}

type FullHash = [u8; 32]; // serialized SHA256 result

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
    fn key(txid: &Sha256dHash, vout: usize) -> Bytes {
        bincode::serialize(&TxOutKey {
            code: b'O',
            txid: txid.to_bytes(),
            vout: vout as u16,
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
        .par_iter()
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

fn get_previous_txos(block_entries: &[BlockEntry]) -> BTreeSet<(Sha256dHash, usize)> {
    block_entries
        .iter()
        .flat_map(|b| {
            b.block.txdata.iter().flat_map(|tx| {
                tx.input.iter().filter_map(|txin| {
                    if txin.previous_output.is_null() {
                        None
                    } else {
                        Some((
                            txin.previous_output.txid,
                            txin.previous_output.vout as usize,
                        ))
                    }
                })
            })
        })
        .collect()
}

fn index_blocks(
    block_entries: &[BlockEntry],
    previous_txos_map: &HashMap<(Sha256dHash, usize), TxOut>,
) -> Vec<DBRow> {
    block_entries
        .par_iter()
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
    previous_txos_map: &HashMap<(Sha256dHash, usize), TxOut>,
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
        let prev_txid = txi.previous_output.txid;
        let prev_vout = txi.previous_output.vout;
        let prev_txo = previous_txos_map
            .get(&(prev_txid, prev_vout as usize))
            .expect(&format!("missing previous txo {}:{}", prev_txid, prev_vout));
        let history = TxHistoryRow::new(
            &prev_txo.script_pubkey,
            confirmed_height,
            TxHistoryInfo::Spending(
                txid,
                txi_index as u16,
                prev_txid.into_bytes(),
                prev_vout as u16,
            ),
        );
        rows.push(history.to_row());

        let edge = TxEdgeRow::new(
            prev_txid.into_bytes(),
            prev_vout as u16,
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
