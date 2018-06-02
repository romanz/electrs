use bincode;
use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::blockdata::transaction::{Transaction, TxIn, TxOut};
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::{deserialize, serialize};
use bitcoin::util::hash::Sha256dHash;
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use pbr;
use std::io::{stderr, Stderr};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use daemon::Daemon;
use store::{ReadStore, Row, WriteStore};
use util::{full_hash, hash_prefix, Bytes, FullHash, HashPrefix, HeaderEntry, HeaderList,
           HeaderMap, Timer, HASH_PREFIX_LEN};

use errors::*;

type ProgressBar = pbr::ProgressBar<Stderr>;

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

    pub fn filter_prefix(txid_prefix: &HashPrefix) -> Bytes {
        [b"T", &txid_prefix[..]].concat()
    }

    pub fn filter_full(txid: &Sha256dHash) -> Bytes {
        [b"T", &txid[..]].concat()
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

pub fn index_transaction(txn: &Transaction, height: usize, rows: &mut Vec<Row>) {
    let null_hash = Sha256dHash::default();
    let txid: Sha256dHash = txn.txid();
    for input in &txn.input {
        if input.prev_hash == null_hash {
            continue;
        }
        rows.push(TxInRow::new(&txid, &input).to_row());
    }
    for output in &txn.output {
        rows.push(TxOutRow::new(&txid, &output).to_row());
    }
    // Persist transaction ID and confirmed height
    rows.push(TxRow::new(&txid, height as u32).to_row());
}

fn index_block(block: &Block, height: usize) -> Vec<Row> {
    let mut rows = vec![];
    for txn in &block.txdata {
        index_transaction(&txn, height, &mut rows);
    }
    let blockhash = block.bitcoin_hash();
    // Persist block hash and header
    rows.push(Row {
        key: bincode::serialize(&BlockKey {
            code: b'B',
            hash: full_hash(&blockhash[..]),
        }).unwrap(),
        value: serialize(&block.header).unwrap(),
    });
    // Store last indexed block (i.e. all previous blocks were indexed)
    rows.push(Row {
        key: b"L".to_vec(),
        value: serialize(&blockhash).unwrap(),
    });
    rows
}

fn read_indexed_headers(store: &ReadStore) -> HeaderList {
    let mut timer = Timer::new();
    let latest_blockhash: Sha256dHash = match store.get(b"L") {
        // latest blockheader persisted in the DB.
        Some(row) => deserialize(&row).unwrap(),
        None => Sha256dHash::default(),
    };
    let mut map = HeaderMap::new();
    for row in store.scan(b"B") {
        let key: BlockKey = bincode::deserialize(&row.key).unwrap();
        let header: BlockHeader = deserialize(&row.value).unwrap();
        map.insert(deserialize(&key.hash).unwrap(), header);
    }
    timer.tick(&format!("reading {} headers from DB", map.len()));
    let mut headers = vec![];
    let null_hash = Sha256dHash::default();
    let mut blockhash = latest_blockhash;
    while blockhash != null_hash {
        let header = map.remove(&blockhash)
            .expect(&format!("missing {} header in DB", blockhash));
        blockhash = header.prev_blockhash;
        headers.push(header);
    }
    headers.reverse();
    assert_eq!(
        headers
            .first()
            .map(|h| h.prev_blockhash)
            .unwrap_or(null_hash),
        null_hash
    );
    assert_eq!(
        headers
            .last()
            .map(|h| h.bitcoin_hash())
            .unwrap_or(null_hash),
        latest_blockhash
    );
    let mut result = HeaderList::empty();
    let entries = result.order(headers);
    result.apply(entries);
    timer.tick("headers' verification");
    result
}

pub struct Index {
    // TODO: store also latest snapshot.
    headers: RwLock<HeaderList>,
}

impl Index {
    pub fn load(store: &ReadStore) -> Index {
        Index {
            headers: RwLock::new(read_indexed_headers(store)),
        }
    }

    pub fn best_header(&self) -> Option<HeaderEntry> {
        let headers = self.headers.read().unwrap();
        headers.header_by_blockhash(headers.tip()).cloned()
    }

    pub fn get_header(&self, height: usize) -> Option<HeaderEntry> {
        self.headers
            .read()
            .unwrap()
            .header_by_height(height)
            .cloned()
    }

    pub fn update(&self, store: &WriteStore, daemon: &Daemon) -> Result<Sha256dHash> {
        let mut timer = Timer::new();
        let tip = daemon.getbestblockhash()?;
        let new_headers = daemon.get_new_headers(&self.headers.read().unwrap(), &tip)?;
        new_headers.last().map(|tip| {
            info!("{:?} ({} left to index)", tip, new_headers.len());
        });
        let mut buf = BufferedWriter::new(store);
        let mut bar = ProgressBar::on(stderr(), new_headers.len() as u64);
        for header in &new_headers {
            let block = daemon.getblock(header.hash())?;
            let rows = index_block(&block, header.height());
            buf.write(rows);
            bar.inc();
        }
        buf.flush(); // make sure no row is left behind
        bar.finish();
        self.headers.write().unwrap().apply(new_headers);
        assert_eq!(tip, *self.headers.read().unwrap().tip());
        timer.tick(&format!("index update ({} blocks)", bar.total));
        Ok(tip)
    }
}

struct BufferedWriter<'a> {
    batch: Vec<Row>,
    start: Instant,
    store: &'a WriteStore,
}

impl<'a> BufferedWriter<'a> {
    fn new(store: &'a WriteStore) -> BufferedWriter {
        BufferedWriter {
            batch: vec![],
            start: Instant::now(),
            store,
        }
    }

    fn write(&mut self, mut rows: Vec<Row>) {
        self.batch.append(&mut rows);
        if self.batch.len() > 10_000_000 || self.start.elapsed() > Duration::from_secs(60) {
            self.flush();
        }
    }

    fn flush(&mut self) {
        self.store.write(self.batch.split_off(0));
        self.start = Instant::now();
    }
}
