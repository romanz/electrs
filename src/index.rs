use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::{deserialize, serialize};
use bitcoin::util::hash::Sha256dHash;
use byteorder::{LittleEndian, WriteBytesExt};
use crypto::digest::Digest;
use crypto::sha2::Sha256;

use daemon::Daemon;
use pbr;
use store::{Row, Store};
use time;
use timer::Timer;
use util;

use {Bytes, HeaderMap};

const HASH_PREFIX_LEN: usize = 8;

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
            key.extend_from_slice(&input.prev_hash[..HASH_PREFIX_LEN]);
            key.write_u16::<LittleEndian>(input.prev_index as u16)
                .unwrap();
            rows.push(Row {
                key: key,
                value: txid[..HASH_PREFIX_LEN].to_vec(),
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
            key.extend_from_slice(&txid[..HASH_PREFIX_LEN]);
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
    headers.retain(|item| !indexed_headers.contains_key(&item.1.bitcoin_hash()[..]));
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
        timer.start("get");
        let buf: Bytes = daemon.get(&format!("block/{}.bin", util::revhex(&blockhash[..])));

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

        pb.inc();
        debug!(
            "{} @ {}: {:.3}/{:.3} MB, {} rows, {}",
            util::revhex(&blockhash[..]),
            height,
            rows_size as f64 / 1e6_f64,
            blocks_size as f64 / 1e6_f64,
            num_of_rows,
            timer.stats()
        );
    }
    store.flush();
    pb.finish();
}
