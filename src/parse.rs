use bitcoin::blockdata::block::Block;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::SimpleDecoder;
use bitcoin::network::serialize::{deserialize, RawDecoder};
use std::fs;
use std::io::{Cursor, Seek, SeekFrom};
use std::path::PathBuf;
use std::thread;

use daemon::Daemon;
use util::SyncChannel;

use errors::*;

/// An efficient parser for Bitcoin blk*.dat files.
pub struct Parser {
    files: Vec<PathBuf>,
}

impl Parser {
    pub fn new(daemon: &Daemon) -> Result<Parser> {
        Ok(Parser {
            files: daemon.list_blk_files()?,
        })
    }

    pub fn start(&self) -> SyncChannel<Result<Vec<Block>>> {
        let chan = SyncChannel::new(1);
        let tx = chan.sender();
        let blobs = read_files(self.files.clone());
        thread::spawn(move || {
            for msg in blobs.receiver().iter() {
                match msg {
                    Ok(blob) => {
                        let blocks = parse_blocks(&blob);
                        tx.send(blocks).unwrap();
                    }
                    Err(err) => {
                        tx.send(Err(err)).unwrap();
                        return;
                    }
                }
            }
        });
        chan
    }
}

fn read_files(files: Vec<PathBuf>) -> SyncChannel<Result<Vec<u8>>> {
    let chan = SyncChannel::new(1);
    let tx = chan.sender();
    thread::spawn(move || {
        info!("reading {} files", files.len());
        for f in &files {
            let msg = fs::read(f).chain_err(|| format!("failed to read {:?}", f));
            debug!("read {:?}", f);
            tx.send(msg).unwrap();
        }
    });
    chan
}

fn parse_blocks(data: &[u8]) -> Result<Vec<Block>> {
    let mut cursor = Cursor::new(&data);
    let mut blocks = vec![];
    let max_pos = data.len() as u64;
    while cursor.position() < max_pos {
        let mut decoder = RawDecoder::new(cursor);
        match decoder.read_u32().chain_err(|| "no magic")? {
            0 => break,
            0xD9B4BEF9 => (),
            x => bail!("incorrect magic {:x}", x),
        };
        let block_size = decoder.read_u32().chain_err(|| "no block size")?;
        cursor = decoder.into_inner();

        let start = cursor.position() as usize;
        cursor
            .seek(SeekFrom::Current(block_size as i64))
            .chain_err(|| format!("seek {} failed", block_size))?;
        let end = cursor.position() as usize;

        let block: Block = deserialize(&data[start..end])
            .chain_err(|| format!("failed to parse block at {}..{}", start, end))?;
        trace!("block {}, {} bytes", block.bitcoin_hash(), block_size);
        blocks.push(block);
    }
    debug!("parsed {} blocks", blocks.len());
    Ok(blocks)
}
