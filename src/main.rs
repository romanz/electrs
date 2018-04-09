#[macro_use]
extern crate log;

extern crate bitcoin;
extern crate byteorder;
extern crate crypto;
extern crate itertools;
extern crate reqwest;
extern crate rocksdb;
extern crate serde_json;
extern crate simple_logger;
extern crate time;

use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::network::encodable::ConsensusDecodable;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::{deserialize, serialize, RawDecoder};
use bitcoin::util::hash::Sha256dHash;
use byteorder::{LittleEndian, WriteBytesExt};
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use itertools::enumerate;
use rocksdb::{DBCompactionStyle, WriteBatch, DB};
// use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::fmt::Write;
use std::io::Cursor;
use time::{Duration, PreciseTime};

const HEADER_SIZE: usize = 80;
const HASH_LEN: usize = 8;

type HeaderMap = HashMap<String, BlockHeader>;

fn get(resource: &str) -> reqwest::Response {
    let url = format!("http://localhost:8332/rest/{}", resource);
    reqwest::get(&url).unwrap()
}

fn get_bin(resource: &str) -> Vec<u8> {
    let mut buf: Vec<u8> = vec![];
    let mut resp = get(resource);
    resp.copy_to(&mut buf).unwrap();
    buf
}

fn get_headers() -> (HeaderMap, String) {
    let mut headers = HashMap::new();
    let mut blockhash =
        String::from("000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"); // genesis
    info!("loading headers from {}", blockhash);
    loop {
        let data = get_bin(&format!("headers/2000/{}.bin", blockhash));
        let num_of_headers = data.len() / HEADER_SIZE;
        let mut decoder = RawDecoder::new(Cursor::new(data));
        for _ in 0..num_of_headers {
            let header: BlockHeader = ConsensusDecodable::consensus_decode(&mut decoder).unwrap();
            blockhash = header.bitcoin_hash().be_hex_string();
            headers.insert(blockhash.to_string(), header);
        }
        if num_of_headers == 1 {
            break;
        }
    }
    info!("loaded {} headers till {}", headers.len(), blockhash);
    (headers, blockhash)
}

fn enumerate_headers(headers: &HeaderMap, bestblockhash: &str) -> Vec<(usize, String)> {
    let null_hash = Sha256dHash::default().be_hex_string();
    let mut hashes = VecDeque::<String>::new();
    let mut blockhash = bestblockhash.to_string();
    while blockhash != null_hash {
        let header: &BlockHeader = headers.get(&blockhash).unwrap();
        hashes.push_front(blockhash);
        blockhash = header.prev_blockhash.be_hex_string();
    }
    enumerate(hashes).collect()
}

struct Row {
    key: Vec<u8>,
    value: Vec<u8>,
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
            let mut key = Vec::<u8>::new(); // ???
            key.push(b'I');
            key.extend_from_slice(&input.prev_hash[..HASH_LEN]);
            key.write_u16::<LittleEndian>(input.prev_index as u16)
                .unwrap();
            rows.push(Row {
                key: key,
                value: txid[..HASH_LEN].to_vec(),
            });
        }
        for output in &tx.output {
            let mut script_hash = [0u8; 32];
            let mut sha2 = Sha256::new();
            sha2.input(&output.script_pubkey[..]);
            sha2.result(&mut script_hash);

            let mut key = Vec::<u8>::new(); // ???
            key.push(b'O');
            key.extend_from_slice(&script_hash);
            key.extend_from_slice(&txid[..HASH_LEN]);
            rows.push(Row {
                key: key,
                value: vec![],
            });
        }
        // Persist transaction ID and confirmed height
        {
            let mut key = Vec::<u8>::new();
            key.push(b'T');
            key.extend_from_slice(&txid[..]);
            let mut value = Vec::<u8>::new();
            value.write_u32::<LittleEndian>(height as u32).unwrap();
            rows.push(Row {
                key: key,
                value: value,
            })
        }
    }
    // Persist block hash and header
    {
        let mut key = Vec::<u8>::new();
        key.push(b'B');
        key.extend_from_slice(&block.bitcoin_hash()[..]);
        rows.push(Row {
            key: key,
            value: serialize(&block.header).unwrap(),
        })
    }
    rows
}

// fn get_bestblockhash() -> String {
//     let data = get("chaininfo.json").text().unwrap();
//     let val: Value = serde_json::from_str(&data).unwrap();
//     val["bestblockhash"].as_str().unwrap().to_string()
// }

struct Timer {
    durations: HashMap<String, Duration>,
    start: Option<PreciseTime>,
    name: String,
}

impl Timer {
    pub fn new() -> Timer {
        Timer {
            durations: HashMap::new(),
            start: None,
            name: String::from(""),
        }
    }

    pub fn start(&mut self, name: &str) {
        self.start = Some(self.stop());
        self.name = name.to_string();
    }

    pub fn stop(&mut self) -> PreciseTime {
        let now = PreciseTime::now();
        if let Some(start) = self.start {
            let duration = self.durations
                .entry(self.name.to_string())   // ???
                .or_insert(Duration::zero());
            *duration = *duration + start.to(now);
        }
        self.start = None;
        now
    }

    pub fn stats(&self) -> String {
        let mut s = String::new();
        let mut total = 0f64;
        for (k, v) in self.durations.iter() {
            let t = v.num_milliseconds() as f64 / 1e3;
            total += t;
            write!(&mut s, "{}: {:.2}s ", k, t).unwrap();
        }
        write!(&mut s, "total: {:.2}s", total).unwrap();
        return s;
    }
}

struct Store {
    db: DB,
    rows: Vec<Row>,
    start: PreciseTime,
}

impl Store {
    /// Opens a new RocksDB at the specified location.
    pub fn open(path: &str) -> Store {
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        opts.set_compaction_style(DBCompactionStyle::Universal);
        opts.set_max_open_files(256);
        opts.set_use_fsync(false);
        Store {
            db: DB::open(&opts, &path).unwrap(),
            rows: vec![],
            start: PreciseTime::now(),
        }
    }

    pub fn persist(&mut self, mut rows: Vec<Row>) {
        self.rows.append(&mut rows);
        let elapsed: Duration = self.start.to(PreciseTime::now());
        if elapsed < Duration::seconds(60) && self.rows.len() < 1_000_000 {
            return;
        }
        let mut batch = WriteBatch::default();
        for row in &self.rows {
            batch.put(row.key.as_slice(), row.value.as_slice()).unwrap();
        }
        self.db.write(batch).unwrap();
        self.rows.clear();
        self.start = PreciseTime::now();
    }
}

fn main() {
    simple_logger::init_with_level(log::Level::Info).unwrap();
    let (headers, blockhash) = get_headers();
    let hashes = enumerate_headers(&headers, &blockhash);
    info!("loading {} blocks", hashes.len());

    let mut timer = Timer::new();

    let mut blocks_size = 0usize;
    let mut rows_size = 0usize;
    let mut num_of_rows = 0usize;

    let mut store = Store::open("db/mainnet");

    for &(height, ref blockhash) in &hashes {
        timer.start("get");
        let buf = get_bin(&format!("block/{}.bin", &blockhash));

        timer.start("parse");
        let block: Block = deserialize(buf.as_slice()).unwrap();

        timer.start("index");
        let rows = index_block(&block, height);
        for row in &rows {
            rows_size += row.key.len() + row.value.len();
        }
        num_of_rows += rows.len();

        timer.start("store");
        store.persist(rows);

        timer.stop();
        blocks_size += buf.len();
        assert_eq!(&block.bitcoin_hash().be_hex_string(), blockhash);
        if height % 100 == 0 {
            info!(
                "{} @ {}: {:.3}/{:.3} MB, {} rows, {}",
                blockhash,
                height,
                rows_size as f64 / 1e6_f64,
                blocks_size as f64 / 1e6_f64,
                num_of_rows,
                timer.stats()
            );
        }
    }
}
