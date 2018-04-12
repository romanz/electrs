use bincode;
use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::blockdata::transaction as txn;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::{deserialize, serialize};
use bitcoin::util::hash::Sha256dHash;
use crypto::digest::Digest;
use crypto::sha2::Sha256;

use daemon::Daemon;
use pbr;
use store::{Row, Store};
use time;
use timer::Timer;

use {Bytes, HeaderMap};

const HASH_LEN: usize = 32;
const HASH_PREFIX_LEN: usize = 8;

type FullHash = [u8; HASH_LEN];
type HashPrefix = [u8; HASH_PREFIX_LEN];

#[derive(Serialize, Deserialize)]
struct TxInKey {
    code: u8,
    prev_hash_prefix: HashPrefix,
    prev_index: u16,
}

#[derive(Serialize, Deserialize)]
struct TxInValue {
    txid_prefix: HashPrefix,
}

fn hash_prefix(hash: &Sha256dHash) -> HashPrefix {
    array_ref![hash, 0, HASH_PREFIX_LEN].clone()
}

fn full_hash(hash: &Sha256dHash) -> FullHash {
    array_ref![hash, 0, HASH_LEN].clone()
}

#[derive(Serialize, Deserialize)]
struct TxOutKey {
    code: u8,
    script_hash: FullHash,
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

#[derive(Serialize, Deserialize)]
struct TxValue {
    confirmed_height: u32,
}

fn digest(data: &[u8]) -> FullHash {
    let mut sha2 = Sha256::new();
    sha2.input(data);

    let mut hash = [0u8; HASH_LEN];
    sha2.result(&mut hash);
    hash
}

fn txin_row(input: &txn::TxIn, txid: &Sha256dHash) -> Row {
    Row {
        key: bincode::serialize(&TxInKey {
            code: b'I',
            prev_hash_prefix: hash_prefix(&input.prev_hash),
            prev_index: input.prev_index as u16,
        }).unwrap(),
        value: bincode::serialize(&TxInValue {
            txid_prefix: hash_prefix(&txid),
        }).unwrap(),
    }
}

fn txout_row(output: &txn::TxOut, txid: &Sha256dHash) -> Row {
    Row {
        key: bincode::serialize(&TxOutKey {
            code: b'O',
            script_hash: digest(&output.script_pubkey[..]),
            txid_prefix: hash_prefix(&txid),
        }).unwrap(),
        value: vec![],
    }
}

fn tx_row(txid: &Sha256dHash, height: usize) -> Row {
    Row {
        key: bincode::serialize(&TxKey {
            code: b'T',
            txid: full_hash(&txid),
        }).unwrap(),
        value: bincode::serialize(&TxValue {
            confirmed_height: height as u32,
        }).unwrap(),
    }
}

fn block_row(block: &Block) -> Row {
    let blockhash = block.bitcoin_hash();
    Row {
        key: bincode::serialize(&BlockKey {
            code: b'B',
            hash: full_hash(&blockhash),
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

fn get_missing_headers(store: &Store, daemon: &Daemon) -> Vec<(usize, BlockHeader)> {
    let indexed_headers: HeaderMap = store.read_headers();
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

pub fn update(store: &mut Store, daemon: &Daemon) {
    let headers = get_missing_headers(store, daemon);
    if headers.is_empty() {
        return;
    }

    let mut timer = Timer::new();

    let mut blocks_size = 0usize;
    let mut rows_size = 0usize;
    let mut num_of_rows = 0usize;

    let mut pb = pbr::ProgressBar::new(headers.len() as u64);
    for (height, header) in headers {
        let blockhash = header.bitcoin_hash();
        let blockhash_hex = blockhash.be_hex_string();

        timer.start("get");
        let buf: Bytes = daemon.get(&format!("block/{}.bin", blockhash_hex));

        timer.start("parse");
        let block: Block = deserialize(&buf).unwrap();
        assert_eq!(block.bitcoin_hash(), blockhash);

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

        if pb.inc() % 100 == 0 {
            info!(
                "{} @ {}: {:.3}/{:.3} MB, {} rows, {}",
                blockhash_hex,
                height,
                rows_size as f64 / 1e6_f64,
                blocks_size as f64 / 1e6_f64,
                num_of_rows,
                timer.stats()
            );
        }
    }
    store.flush();
    pb.finish();
}
