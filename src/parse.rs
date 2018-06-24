use bitcoin::blockdata::block::Block;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::SimpleDecoder;
use bitcoin::network::serialize::{deserialize, RawDecoder};
use bitcoin::util::hash::Sha256dHash;
use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Seek, SeekFrom};
use std::iter::FromIterator;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use daemon::Daemon;
use index::index_block;
use metrics::{HistogramOpts, HistogramVec, Metrics};
use store::Row;
use util::{spawn_thread, HeaderEntry, SyncChannel};

use errors::*;

/// An efficient parser for Bitcoin blk*.dat files.
pub struct Parser {
    files: Vec<PathBuf>,
    // metrics
    duration: HistogramVec,
}

impl Parser {
    pub fn new(daemon: &Daemon, metrics: &Metrics) -> Result<Parser> {
        Ok(Parser {
            files: daemon.list_blk_files()?,
            duration: metrics.histogram_vec(
                HistogramOpts::new("parse_duration", "Block parsing duration (in seconds)"),
                &["step"],
            ),
        })
    }

    pub fn start(self, headers: Vec<HeaderEntry>) -> Receiver<Result<Vec<Row>>> {
        let height_map = HashMap::<Sha256dHash, usize>::from_iter(
            headers.iter().map(|h| (*h.hash(), h.height())),
        );
        let chan = SyncChannel::new(1);
        let tx = chan.sender();
        let parser = parse_files(self.files.clone(), self.duration.clone());
        let duration = self.duration.clone();
        spawn_thread("bulk_indexer", move || {
            for msg in parser.iter() {
                match msg {
                    Ok(blocks) => {
                        let mut rows = vec![];
                        for block in &blocks {
                            let blockhash = block.bitcoin_hash();
                            if let Some(height) = height_map.get(&blockhash) {
                                let timer = duration.with_label_values(&["index"]).start_timer();
                                rows.extend(index_block(block, *height));
                                timer.observe_duration();
                            } else {
                                warn!("unknown block {}", blockhash);
                            }
                        }
                        trace!("indexed {} rows from {} blocks", rows.len(), blocks.len());
                        tx.send(Ok(rows)).unwrap();
                    }
                    Err(err) => {
                        tx.send(Err(err)).unwrap();
                    }
                }
            }
        });
        chan.into_receiver()
    }
}

fn parse_files(files: Vec<PathBuf>, duration: HistogramVec) -> Receiver<Result<Vec<Block>>> {
    let chan = SyncChannel::new(1);
    let tx = chan.sender();
    let blobs = read_files(files, duration.clone());
    let duration = duration.clone();
    spawn_thread("bulk_parser", move || {
        for msg in blobs.iter() {
            match msg {
                Ok(blob) => {
                    let timer = duration.with_label_values(&["parse"]).start_timer();
                    let blocks = parse_blocks(&blob);
                    timer.observe_duration();
                    tx.send(blocks).unwrap();
                }
                Err(err) => {
                    tx.send(Err(err)).unwrap();
                }
            }
        }
    });
    chan.into_receiver()
}

fn read_files(files: Vec<PathBuf>, duration: HistogramVec) -> Receiver<Result<Vec<u8>>> {
    let chan = SyncChannel::new(1);
    let tx = chan.sender();
    spawn_thread("bulk_reader", move || {
        for f in &files {
            let timer = duration.with_label_values(&["read"]).start_timer();
            let msg = fs::read(f).chain_err(|| format!("failed to read {:?}", f));
            timer.observe_duration();
            if let Ok(ref blob) = msg {
                trace!("read {:.2} MB from {:?}", blob.len() as f32 / 1e6, f);
            }
            tx.send(msg).unwrap();
        }
        debug!("read {} blk files", files.len());
    });
    chan.into_receiver()
}

fn parse_blocks(blob: &[u8]) -> Result<Vec<Block>> {
    let mut cursor = Cursor::new(&blob);
    let mut blocks = vec![];
    let max_pos = blob.len() as u64;
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

        let block: Block = deserialize(&blob[start..end])
            .chain_err(|| format!("failed to parse block at {}..{}", start, end))?;
        blocks.push(block);
    }
    trace!(
        "parsed {} blocks from {:.2} MB blob",
        blocks.len(),
        blob.len() as f32 / 1e6
    );
    Ok(blocks)
}
