use bincode;
use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::blockdata::transaction::{Transaction, TxIn, TxOut};
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::{deserialize, serialize};
use bitcoin::util::hash::Sha256dHash;
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use itertools::enumerate;
use pbr;
use std::io::{stderr, Stderr};
use std::time::{Duration, Instant};
use time;

use daemon::Daemon;
use store::{Row, Store};
use types::{Bytes, HeaderMap};

const HASH_LEN: usize = 32;
const HASH_PREFIX_LEN: usize = 8;

type FullHash = [u8; HASH_LEN];
type HashPrefix = [u8; HASH_PREFIX_LEN];

fn hash_prefix(hash: &[u8]) -> HashPrefix {
    array_ref![hash, 0, HASH_PREFIX_LEN].clone()
}

fn full_hash(hash: &[u8]) -> FullHash {
    array_ref![hash, 0, HASH_LEN].clone()
}

#[derive(Serialize, Deserialize)]
struct TxInKey {
    code: u8,
    prev_hash_prefix: HashPrefix,
    prev_index: u16,
}

#[derive(Serialize, Deserialize)]
struct TxInRow {
    key: TxInKey,
    txid_prefix: HashPrefix,
}

#[derive(Serialize, Deserialize)]
struct TxOutKey {
    code: u8,
    script_hash_prefix: HashPrefix,
}

#[derive(Serialize, Deserialize)]
struct TxOutRow {
    key: TxOutKey,
    txid_prefix: HashPrefix,
}

#[derive(Serialize, Deserialize)]
struct TxKey {
    code: u8,
    txid: FullHash,
}

#[derive(Serialize, Deserialize)]
struct BlockKey {
    code: u8,
    hash: FullHash,
}

fn digest(data: &[u8]) -> FullHash {
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
                script_hash_prefix: hash_prefix(&digest(&output.script_pubkey[..])),
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

fn read_headers(store: &Store) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for row in store.scan(b"B") {
        let key: BlockKey = bincode::deserialize(&row.key).unwrap();
        let header: BlockHeader = deserialize(&row.value).unwrap();
        headers.insert(deserialize(&key.hash).unwrap(), header);
    }
    headers
}

fn get_missing_headers(store: &Store, daemon: &Daemon) -> Vec<(usize, BlockHeader)> {
    let indexed_headers: HeaderMap = read_headers(&store);
    let mut headers: Vec<(usize, BlockHeader)> = daemon.enumerate_headers();
    {
        let best_block_header = &headers.last().unwrap().1;
        info!(
            "got {} headers (indexed {}), best {} @ {}",
            headers.len(),
            indexed_headers.len(),
            best_block_header.bitcoin_hash(),
            time::at_utc(time::Timespec::new(best_block_header.time as i64, 0)).rfc3339(),
        );
    }
    headers.retain(|item| !indexed_headers.contains_key(&item.1.bitcoin_hash()));
    headers
}

struct Indexer<'a> {
    headers: Vec<(usize, BlockHeader)>,
    header_index: usize,

    daemon: &'a Daemon,

    blocks_size: usize,
    rows_size: usize,
    num_of_rows: usize,
}

impl<'a> Indexer<'a> {
    fn new(store: &Store, daemon: &'a Daemon) -> Indexer<'a> {
        let headers = get_missing_headers(store, daemon);
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
        let &(height, header) = &self.headers[self.header_index];

        let blockhash = header.bitcoin_hash();
        let blockhash_hex = blockhash.be_hex_string();

        let buf: Bytes = self.daemon.get(&format!("block/{}.bin", blockhash_hex));

        let block: Block = deserialize(&buf).unwrap();
        assert_eq!(block.bitcoin_hash(), blockhash);

        let rows = index_block(&block, height);

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
                height,
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
    fn new(indexer: Indexer) -> BatchIter {
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

pub fn update(store: &Store, daemon: &Daemon) {
    let indexer = Indexer::new(&store, &daemon);
    for rows in BatchIter::new(indexer) {
        // TODO: add timing
        store.persist(&rows);
    }
}

pub struct Query<'a> {
    store: &'a Store,
    daemon: &'a Daemon,
}

impl<'a> Query<'a> {
    pub fn new(store: &'a Store, daemon: &'a Daemon) -> Query<'a> {
        Query { store, daemon }
    }

    fn load_txns(&self, prefixes: Vec<HashPrefix>) -> Vec<Transaction> {
        let mut txns = Vec::new();
        for txid_prefix in prefixes {
            for row in self.store.scan(&[b"T", &txid_prefix[..]].concat()) {
                let key: TxKey = bincode::deserialize(&row.key).unwrap();
                let txid: Sha256dHash = deserialize(&key.txid).unwrap();
                let txn_bytes = self.daemon.get(&format!("tx/{}.bin", txid.be_hex_string()));
                let txn: Transaction = deserialize(&txn_bytes).unwrap();
                txns.push(txn)
            }
        }
        txns
    }

    fn find_spending_txn(&self, txid: &Sha256dHash, output_index: u32) -> Option<Transaction> {
        let spend_key = bincode::serialize(&TxInKey {
            code: b'I',
            prev_hash_prefix: hash_prefix(&txid[..]),
            prev_index: output_index as u16,
        }).unwrap();
        let mut spending: Vec<Transaction> = self.load_txns(
            self.store
                .scan(&spend_key)
                .iter()
                .map(|row| {
                    bincode::deserialize::<TxInRow>(&row.key)
                        .unwrap()
                        .txid_prefix
                })
                .collect(),
        );
        spending.retain(|item| {
            item.input
                .iter()
                .any(|input| input.prev_hash == *txid && input.prev_index == output_index)
        });
        assert!(spending.len() <= 1);
        if spending.len() == 1 {
            Some(spending.remove(0))
        } else {
            None
        }
    }

    pub fn balance(&self, script_hash: &[u8]) -> f64 {
        let mut funding: Vec<Transaction> = self.load_txns(
            self.store
                .scan(&[b"O", &script_hash[..HASH_PREFIX_LEN]].concat())
                .iter()
                .map(|row| {
                    bincode::deserialize::<TxOutRow>(&row.key)
                        .unwrap()
                        .txid_prefix
                })
                .collect(),
        );
        funding.retain(|item| {
            item.output
                .iter()
                .any(|output| digest(&output.script_pubkey[..]) == script_hash)
        });

        let mut balance = 0u64;
        let mut spending = Vec::<Transaction>::new();
        for txn in &funding {
            let txid = txn.txid();
            for (index, output) in enumerate(&txn.output) {
                if let Some(spent) = self.find_spending_txn(&txid, index as u32) {
                    spending.push(spent); // TODO: may contain duplicate TXNs
                } else {
                    balance += output.value;
                }
            }
        }
        balance as f64 / 100_000_000f64
    }
}
