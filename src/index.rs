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
use std::fmt;
use std::io::{stderr, Stderr};
use std::iter::FromIterator;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use time;

use daemon::Daemon;
use store::{Row, Store};
use types::{full_hash, hash_prefix, Bytes, FullHash, HashPrefix, HeaderMap, HASH_PREFIX_LEN};

type ProgressBar = pbr::ProgressBar<Stderr>;

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
    tip: Sha256dHash,
}

impl HeaderList {
    pub fn build(mut header_map: HeaderMap, mut blockhash: Sha256dHash) -> HeaderList {
        let null_hash = Sha256dHash::default();
        let tip = blockhash;
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
            tip: tip,
        }
    }

    pub fn empty() -> HeaderList {
        HeaderList {
            headers: vec![],
            tip: Sha256dHash::default(),
        }
    }

    pub fn equals(&self, other: &HeaderList) -> bool {
        self.headers.last() == other.headers.last()
    }

    pub fn headers(&self) -> &[HeaderEntry] {
        &self.headers
    }

    pub fn tip(&self) -> Sha256dHash {
        self.tip
    }

    pub fn height(&self) -> usize {
        self.headers.len() - 1
    }

    pub fn as_map(&self) -> HeaderMap {
        HeaderMap::from_iter(self.headers.iter().map(|entry| (entry.hash, entry.header)))
    }
}

impl fmt::Debug for HeaderList {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let last_block_time = self.headers.last().map_or("N/A".to_string(), |h| {
            time::at_utc(time::Timespec::new(h.header.time as i64, 0))
                .rfc3339()
                .to_string()
        });

        write!(
            f,
            "height {}, best {} @ {}",
            self.height(),
            self.tip(),
            last_block_time,
        )
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

impl TxInRow {
    pub fn new(txid: &Sha256dHash, input: &TxIn) -> TxInRow {
        TxInRow {
            key: TxInKey {
                code: b'I',
                prev_hash_prefix: hash_prefix(&input.prev_hash[..]),
                prev_index: input.prev_index as u16,
            },
            txid_prefix: hash_prefix(&txid[..]),
        }
    }

    pub fn filter(txid: &Sha256dHash, output_index: usize) -> Bytes {
        bincode::serialize(&TxInKey {
            code: b'I',
            prev_hash_prefix: hash_prefix(&txid[..]),
            prev_index: output_index as u16,
        }).unwrap()
    }

    pub fn to_row(&self) -> Row {
        Row {
            key: bincode::serialize(&self).unwrap(),
            value: vec![],
        }
    }

    pub fn from_row(row: &Row) -> TxInRow {
        bincode::deserialize(&row.key).expect("failed to parse TxInRow")
    }
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

impl TxOutRow {
    pub fn new(txid: &Sha256dHash, output: &TxOut) -> TxOutRow {
        TxOutRow {
            key: TxOutKey {
                code: b'O',
                script_hash_prefix: hash_prefix(&compute_script_hash(&output.script_pubkey[..])),
            },
            txid_prefix: hash_prefix(&txid[..]),
        }
    }

    pub fn filter(script_hash: &[u8]) -> Bytes {
        bincode::serialize(&TxOutKey {
            code: b'O',
            script_hash_prefix: hash_prefix(&script_hash[..HASH_PREFIX_LEN]),
        }).unwrap()
    }

    pub fn to_row(&self) -> Row {
        Row {
            key: bincode::serialize(&self).unwrap(),
            value: vec![],
        }
    }

    pub fn from_row(row: &Row) -> TxOutRow {
        bincode::deserialize(&row.key).expect("failed to parse TxOutRow")
    }
}

#[derive(Serialize, Deserialize)]
pub struct TxKey {
    code: u8,
    pub txid: FullHash,
}

pub struct TxRow {
    pub key: TxKey,
    pub height: u32, // value
}

impl TxRow {
    pub fn new(txid: &Sha256dHash, height: u32) -> TxRow {
        TxRow {
            key: TxKey {
                code: b'T',
                txid: full_hash(&txid[..]),
            },
            height: height,
        }
    }

    pub fn filter(txid_prefix: &HashPrefix) -> Bytes {
        [b"T", &txid_prefix[..]].concat()
    }

    pub fn to_row(&self) -> Row {
        Row {
            key: bincode::serialize(&self.key).unwrap(),
            value: bincode::serialize(&self.height).unwrap(),
        }
    }

    pub fn from_row(row: &Row) -> TxRow {
        TxRow {
            key: bincode::deserialize(&row.key).expect("failed to parse TxKey"),
            height: bincode::deserialize(&row.value).expect("failed to parse height"),
        }
    }
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
            rows.push(TxInRow::new(&txid, &input).to_row());
        }
        for output in &tx.output {
            rows.push(TxOutRow::new(&txid, &output).to_row());
        }
        // Persist transaction ID and confirmed height
        rows.push(TxRow::new(&txid, height as u32).to_row());
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
    bar: Option<ProgressBar>,
}

impl<'a> Indexer<'a> {
    fn new(
        headers: Vec<&'a HeaderEntry>,
        daemon: &'a Daemon,
        use_progress_bar: bool,
    ) -> Indexer<'a> {
        let bar = if use_progress_bar {
            Some(ProgressBar::on(stderr(), headers.len() as u64))
        } else {
            None
        };
        Indexer {
            headers: headers,
            header_index: 0,

            daemon: daemon,

            blocks_size: 0,
            rows_size: 0,
            num_of_rows: 0,
            bar: bar,
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
            self.bar.as_mut().map(|b| b.finish());
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
        self.bar.as_mut().map(|b| b.inc());
        Some(rows)
    }
}

struct Batching<'a> {
    indexer: Indexer<'a>,
    batch: Vec<Row>,
    start: Instant,
}

impl<'a> Batching<'a> {
    fn new(indexer: Indexer<'a>) -> Batching<'a> {
        Batching {
            indexer: indexer,
            batch: vec![],
            start: Instant::now(),
        }
    }
}

impl<'a> Iterator for Batching<'a> {
    type Item = Vec<Row>;

    fn next(&mut self) -> Option<Vec<Row>> {
        const MAX_ELAPSED: Duration = Duration::from_secs(60);
        const MAX_BATCH_LEN: usize = 10_000_000;

        while let Some(mut rows) = self.indexer.next() {
            self.batch.append(&mut rows);
            if self.batch.len() > MAX_BATCH_LEN || self.start.elapsed() > MAX_ELAPSED {
                break;
            }
        }
        self.start = Instant::now();
        if self.batch.is_empty() {
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
            "{:?} ({} left to index)",
            current_headers,
            missing_headers.len(),
        );
        missing_headers
    }

    pub fn update(&self, store: &Store, daemon: &Daemon) -> Sha256dHash {
        let mut indexed_headers: Arc<HeaderList> = self.headers_list();
        let first_invocation = indexed_headers.headers().is_empty();
        if first_invocation {
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
            /*use_progress_bar=*/ first_invocation,
        )) {
            // TODO: add timing
            store.persist(&rows);
        }
        let tip = current_headers.tip();
        *(self.headers.write().unwrap()) = Arc::new(current_headers);
        tip
    }
}
