use bitcoin::blockdata::block::Block;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::SimpleDecoder;
use bitcoin::network::serialize::{deserialize, RawDecoder};
use std::fs;
use std::io::{Cursor, Seek, SeekFrom};
use std::path::PathBuf;
use std::thread;
use std::time::Instant;

use daemon::{list_blk_files, Network};
use util::SyncChannel;

use errors::*;

/// An efficient parser for Bitcoin blk*.dat files.
pub struct Parser {
    files: Vec<PathBuf>,
}

impl Parser {
    pub fn new(network: Network) -> Result<Parser> {
        Ok(Parser {
            files: list_blk_files(network)?,
        })
    }

    pub fn run(&self) -> Result<()> {
        info!("reading {} files", self.files.len());
        let blobs = read_files(&self.files);
        for blob in blobs.receiver().iter() {
            let t = Instant::now();
            let blocks = parse_blk_file(&blob?);
            debug!("parsing {} blocks took {:?}", blocks.len(), t.elapsed());
        }
        Ok(())
    }
}

fn read_files(files: &[PathBuf]) -> SyncChannel<Result<Vec<u8>>> {
    let chan = SyncChannel::new(1);
    let tx = chan.sender();
    let files = files.to_vec();
    thread::spawn(move || {
        for f in &files {
            let t = Instant::now();
            let msg = fs::read(f).chain_err(|| format!("failed to read {:?}", f));
            debug!("reading {:?} took {:?}", f, t.elapsed());
            tx.send(msg).unwrap();
        }
    });
    chan
}

fn parse_blk_file(data: &[u8]) -> Vec<Block> {
    let mut cursor = Cursor::new(&data);
    let mut result = vec![];
    let max_pos = data.len() as u64;
    while cursor.position() < max_pos {
        let mut decoder = RawDecoder::new(cursor);
        match decoder.read_u32().expect("no magic") {
            0 => break,
            x => assert_eq!(x, 0xD9B4BEF9, "incorrect magic {:x}", x),
        };
        let block_size = decoder.read_u32().expect("no block size");
        cursor = decoder.into_inner();

        let start = cursor.position() as usize;
        cursor
            .seek(SeekFrom::Current(block_size as i64))
            .expect(&format!("seek {} failed", block_size));
        let end = cursor.position() as usize;

        let block: Block = deserialize(&data[start..end])
            .expect(&format!("failed to parse block at {}..{}", start, end));
        trace!("block {}, {} bytes", block.bitcoin_hash(), block_size);
        result.push(block);
    }
    result
}
