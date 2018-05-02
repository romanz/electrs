use bincode;
use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::blockdata::transaction::{TxIn, TxOut};
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::{deserialize, serialize};
use bitcoin::util::hash::Sha256dHash;
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use pbr;
use std::io::{stderr, Stderr};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use time;

use daemon::{Daemon, HeaderEntry, HeaderList};
use store::{Row, Store};
use types::{Bytes, HeaderMap};

// TODO: consolidate serialization/deserialize code for bincode/bitcoin.
const HASH_LEN: usize = 32;
pub const HASH_PREFIX_LEN: usize = 8;

pub type FullHash = [u8; HASH_LEN];
pub type HashPrefix = [u8; HASH_PREFIX_LEN];

pub fn hash_prefix(hash: &[u8]) -> HashPrefix {
    array_ref![hash, 0, HASH_PREFIX_LEN].clone()
}

fn full_hash(hash: &[u8]) -> FullHash {
    array_ref![hash, 0, HASH_LEN].clone()
}

#[derive(Serialize, Deserialize)]
pub struct TxInKey {
    pub code: u8,
    pub prev_hash_prefix: HashPrefix,
    pub prev_index: u16,
}

#[derive(Serialize, Deserialize)]
pub struct TxInRow {
    key: TxInKey,
    pub txid_prefix: HashPrefix,
}

#[derive(Serialize, Deserialize)]
pub struct TxOutKey {
    code: u8,
    script_hash_prefix: HashPrefix,
}

#[derive(Serialize, Deserialize)]
pub struct TxOutRow {
    key: TxOutKey,
    pub txid_prefix: HashPrefix,
}

#[derive(Serialize, Deserialize)]
pub struct TxKey {
    code: u8,
    pub txid: FullHash,
}

#[derive(Serialize, Deserialize)]
pub struct BlockKey {
    code: u8,
    hash: FullHash,
}

pub fn compute_script_hash(data: &[u8]) -> FullHash {
    let mut hash = FullHash::default();
    let mut sha2 = Sha256::new();
    sha2.input(data);
    sha2.result(&mut hash);
    hash
}

fn txin_row(input: &TxIn, txid: &Sha256dHash) -> Row {
    Row {
        key: bincode::serialize(&TxInRow {
            key: TxInKey {
                code: b'I',
                prev_hash_prefix: hash_prefix(&input.prev_hash[..]),
                prev_index: input.prev_index as u16,
            },
            txid_prefix: hash_prefix(&txid[..]),
        }).unwrap(),
        value: vec![],
    }
}

fn txout_row(output: &TxOut, txid: &Sha256dHash) -> Row {
    Row {
        key: bincode::serialize(&TxOutRow {
            key: TxOutKey {
                code: b'O',
                script_hash_prefix: hash_prefix(&compute_script_hash(&output.script_pubkey[..])),
            },
            txid_prefix: hash_prefix(&txid[..]),
        }).unwrap(),
        value: vec![],
    }
}

fn tx_row(txid: &Sha256dHash, height: usize) -> Row {
    Row {
        key: bincode::serialize(&TxKey {
            code: b'T',
            txid: full_hash(&txid[..]),
        }).unwrap(),
        value: bincode::serialize(&(height as u32)).unwrap(),
    }
}

fn block_row(block: &Block) -> Row {
    let blockhash = block.bitcoin_hash();
    Row {
        key: bincode::serialize(&BlockKey {
            code: b'B',
            hash: full_hash(&blockhash[..]),
        }).unwrap(),
        value: serialize(&block.header).unwrap(),
    }
}

fn index_block(block: &Block, height: usize) -> Vec<Row> {
    let null_hash = Sha256dHash::default();
    let mut rows = Vec::new();
    for tx in &block.txdata {
        let txid: Sha256dHash = tx.txid();
        for input in &tx.input {
            if input.prev_hash == null_hash {
                continue;
            }
            rows.push(txin_row(&input, &txid));
        }
        for output in &tx.output {
            rows.push(txout_row(&output, &txid))
        }
        // Persist transaction ID and confirmed height
        rows.push(tx_row(&txid, height))
    }
    // Persist block hash and header
    rows.push(block_row(&block));
    rows
}

fn read_indexed_headers(store: &Store) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for row in store.scan(b"B") {
        let key: BlockKey = bincode::deserialize(&row.key).unwrap();
        let header: BlockHeader = deserialize(&row.value).unwrap();
        headers.insert(deserialize(&key.hash).unwrap(), header);
    }
    headers
}

struct Indexer<'a> {
    headers: Vec<&'a HeaderEntry>,
    header_index: usize,

    daemon: &'a Daemon,

    blocks_size: usize,
    rows_size: usize,
    num_of_rows: usize,
}

impl<'a> Indexer<'a> {
    fn new(headers: Vec<&'a HeaderEntry>, daemon: &'a Daemon) -> Indexer<'a> {
        Indexer {
            headers: headers,
            header_index: 0,

            daemon: daemon,

            blocks_size: 0,
            rows_size: 0,
            num_of_rows: 0,
        }
    }

    fn num_of_headers(&self) -> usize {
        self.headers.len()
    }
}

impl<'a> Iterator for Indexer<'a> {
    type Item = Vec<Row>;

    fn next(&mut self) -> Option<Vec<Row>> {
        if self.header_index >= self.num_of_headers() {
            return None;
        }
        let &entry = &self.headers[self.header_index];

        let &blockhash = entry.hash();
        let blockhash_hex = blockhash.be_hex_string();

        let buf: Bytes = self.daemon.get(&format!("block/{}.bin", blockhash_hex));

        let block: Block = deserialize(&buf).unwrap();
        assert_eq!(block.bitcoin_hash(), blockhash);

        let rows = index_block(&block, entry.height());

        self.blocks_size += buf.len();
        for row in &rows {
            self.rows_size += row.key.len() + row.value.len();
        }
        self.num_of_rows += rows.len();
        self.header_index += 1;

        if self.header_index % 1000 == 0 {
            info!(
                "{} @ {}: {:.3}/{:.3} MB, {} rows",
                blockhash_hex,
                entry.height(),
                self.rows_size as f64 / 1e6_f64,
                self.blocks_size as f64 / 1e6_f64,
                self.num_of_rows,
            );
        }
        Some(rows)
    }
}

struct BatchIter<'a> {
    indexer: Indexer<'a>,
    batch: Vec<Row>,
    start: Instant,
    bar: pbr::ProgressBar<Stderr>,
}

impl<'a> BatchIter<'a> {
    fn new(indexer: Indexer<'a>) -> BatchIter<'a> {
        let bar = pbr::ProgressBar::on(stderr(), indexer.num_of_headers() as u64);
        BatchIter {
            indexer: indexer,
            batch: vec![],
            start: Instant::now(),
            bar: bar,
        }
    }
}

impl<'a> Iterator for BatchIter<'a> {
    type Item = Vec<Row>;

    fn next(&mut self) -> Option<Vec<Row>> {
        while let Some(mut rows) = self.indexer.next() {
            self.bar.inc();
            self.batch.append(&mut rows);
            if self.batch.len() > 10_000_000 || self.start.elapsed() > Duration::from_secs(60) {
                break;
            }
        }
        self.start = Instant::now();
        if self.batch.is_empty() {
            self.bar.finish();
            None
        } else {
            Some(self.batch.split_off(0))
        }
    }
}

pub struct Index {
    // TODO: store also a &HeaderMap.
    // TODO: store also latest snapshot.
    headers: RwLock<Arc<HeaderList>>,
}

impl Index {
    pub fn new() -> Index {
        Index {
            headers: RwLock::new(Arc::new(HeaderList::empty())),
        }
    }

    pub fn headers_list(&self) -> Arc<HeaderList> {
        self.headers.read().unwrap().clone()
    }

    fn get_missing_headers<'a>(
        &self,
        store: &Store,
        current_headers: &'a HeaderList,
    ) -> Vec<&'a HeaderEntry> {
        let indexed_headers: HeaderMap = read_indexed_headers(&store);
        {
            let best_block_header: &BlockHeader =
                current_headers.headers().last().unwrap().header();
            info!(
                "height {}, best {} @ {} ({} left to index)",
                current_headers.headers().len() - 1,
                best_block_header.bitcoin_hash(),
                time::at_utc(time::Timespec::new(best_block_header.time as i64, 0)).rfc3339(),
                current_headers.headers().len() - indexed_headers.len(),
            );
        }
        current_headers
            .headers()
            .iter()
            .filter(|entry| !indexed_headers.contains_key(&entry.hash()))
            .collect()
    }

    pub fn update(&self, store: &Store, daemon: &Daemon) {
        let indexed_headers: Arc<HeaderList> = self.headers_list();
        let current_headers = daemon.enumerate_headers(&*indexed_headers);
        {
            if indexed_headers.equals(&current_headers) {
                return; // everything was indexed already.
            }
            let missing_headers = self.get_missing_headers(&store, &current_headers);
            for rows in BatchIter::new(Indexer::new(missing_headers, &daemon)) {
                // TODO: add timing
                store.persist(&rows);
            }
        }
        *self.headers.write().unwrap() = Arc::new(current_headers);
    }
}
