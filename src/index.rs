use bincode;
use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::blockdata::transaction::{TxIn, TxOut};
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::{deserialize, serialize};
use bitcoin::util::hash::Sha256dHash;
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use log::Level;
use pbr;
use std::collections::VecDeque;
use std::io::{stderr, Stderr};
use std::iter::FromIterator;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use time;

use store::{Row, Store};
use types::HeaderMap;
use daemon::Daemon;

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

// TODO: move to a separate file (to break index<->daemon dependency)
#[derive(Eq, PartialEq, Clone)]
pub struct HeaderEntry {
    height: usize,
    hash: Sha256dHash,
    header: BlockHeader,
}

impl HeaderEntry {
    pub fn hash(&self) -> &Sha256dHash {
        &self.hash
    }

    pub fn header(&self) -> &BlockHeader {
        &self.header
    }

    pub fn height(&self) -> usize {
        self.height
    }
}

pub struct HeaderList {
    headers: Vec<HeaderEntry>,
}

impl HeaderList {
    pub fn build(mut header_map: HeaderMap, mut blockhash: Sha256dHash) -> HeaderList {
        let null_hash = Sha256dHash::default();

        struct HashedHeader {
            blockhash: Sha256dHash,
            header: BlockHeader,
        }
        let mut hashed_headers = VecDeque::<HashedHeader>::new();
        while blockhash != null_hash {
            let header: BlockHeader = header_map.remove(&blockhash).unwrap();
            hashed_headers.push_front(HashedHeader { blockhash, header });
            blockhash = header.prev_blockhash;
        }
        assert!(header_map.is_empty());
        HeaderList {
            headers: hashed_headers
                .into_iter()
                .enumerate()
                .map(|(height, hashed_header)| HeaderEntry {
                    height: height,
                    hash: hashed_header.blockhash,
                    header: hashed_header.header,
                })
                .collect(),
        }
    }

    pub fn empty() -> HeaderList {
        HeaderList { headers: vec![] }
    }

    pub fn equals(&self, other: &HeaderList) -> bool {
        self.headers.last() == other.headers.last()
    }

    pub fn headers(&self) -> &[HeaderEntry] {
        &self.headers
    }

    pub fn as_map(&self) -> HeaderMap {
        HeaderMap::from_iter(self.headers.iter().map(|entry| (entry.hash, entry.header)))
    }
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

fn block_rows(block: &Block) -> Vec<Row> {
    let blockhash = block.bitcoin_hash();
    vec![
        Row {
            key: bincode::serialize(&BlockKey {
                code: b'B',
                hash: full_hash(&blockhash[..]),
            }).unwrap(),
            value: serialize(&block.header).unwrap(),
        },
        Row {
            key: b"L".to_vec(),
            value: serialize(&blockhash).unwrap(),
        },
    ]
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
    rows.extend(block_rows(&block));
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

fn read_last_indexed_blockhash(store: &Store) -> Option<Sha256dHash> {
    let row = store.get(b"L")?;
    let blockhash: Sha256dHash = deserialize(&row).unwrap();
    Some(blockhash)
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

        let blockhash = entry.hash();
        let block = self.daemon.getblock(&blockhash).unwrap();
        assert_eq!(block.bitcoin_hash(), *blockhash);
        self.blocks_size += serialize(&block).unwrap().len();

        let rows = index_block(&block, entry.height());
        self.num_of_rows += rows.len();
        for row in &rows {
            self.rows_size += row.key.len() + row.value.len();
        }

        self.header_index += 1;
        if self.header_index % 1000 == 0 {
            info!(
                "{} @ {}: {:.3}/{:.3} MB, {} rows",
                blockhash,
                entry.height(),
                self.rows_size as f64 / 1e6_f64,
                self.blocks_size as f64 / 1e6_f64,
                self.num_of_rows,
            );
        }
        Some(rows)
    }
}

struct Batching<'a> {
    indexer: Indexer<'a>,
    batch: Vec<Row>,
    start: Instant,
    bar: pbr::ProgressBar<Stderr>,
}

impl<'a> Batching<'a> {
    fn new(indexer: Indexer<'a>) -> Batching<'a> {
        let bar = pbr::ProgressBar::on(stderr(), indexer.num_of_headers() as u64);
        Batching {
            indexer: indexer,
            batch: vec![],
            start: Instant::now(),
            bar: bar,
        }
    }
}

impl<'a> Iterator for Batching<'a> {
    type Item = Vec<Row>;

    fn next(&mut self) -> Option<Vec<Row>> {
        const MAX_ELAPSED: Duration = Duration::from_secs(60);
        const MAX_BATCH_LEN: usize = 10_000_000;

        while let Some(mut rows) = self.indexer.next() {
            self.bar.inc();
            self.batch.append(&mut rows);
            if self.batch.len() > MAX_BATCH_LEN || self.start.elapsed() > MAX_ELAPSED {
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
        indexed_headers: &HeaderMap,
        current_headers: &'a HeaderList,
    ) -> Vec<&'a HeaderEntry> {
        let best_block_header: &BlockHeader = current_headers.headers().last().unwrap().header();
        let missing_headers: Vec<&'a HeaderEntry> = current_headers
            .headers()
            .iter()
            .filter(|entry| !indexed_headers.contains_key(&entry.hash()))
            .collect();
        log!(
            if missing_headers.is_empty() {
                Level::Debug
            } else {
                Level::Info
            },
            "height {}, best {} @ {} ({} left to index)",
            current_headers.headers().len() - 1,
            best_block_header.bitcoin_hash(),
            time::at_utc(time::Timespec::new(best_block_header.time as i64, 0)).rfc3339(),
            missing_headers.len(),
        );
        missing_headers
    }

    pub fn update(&self, store: &Store, daemon: &Daemon) {
        let mut indexed_headers: Arc<HeaderList> = self.headers_list();
        if indexed_headers.headers().is_empty() {
            if let Some(last_blockhash) = read_last_indexed_blockhash(&store) {
                indexed_headers = Arc::new(HeaderList::build(
                    read_indexed_headers(&store),
                    last_blockhash,
                ));
            }
        }
        let current_headers = daemon.enumerate_headers(&*indexed_headers).unwrap();
        for rows in Batching::new(Indexer::new(
            self.get_missing_headers(&indexed_headers.as_map(), &current_headers),
            &daemon,
        )) {
            // TODO: add timing
            store.persist(&rows);
        }
        *self.headers.write().unwrap() = Arc::new(current_headers);
    }
}
