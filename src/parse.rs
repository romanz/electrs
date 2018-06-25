use bitcoin::blockdata::block::Block;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::SimpleDecoder;
use bitcoin::network::serialize::{deserialize, RawDecoder};
use bitcoin::util::hash::Sha256dHash;
use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use daemon::Daemon;
use index::{index_block, last_indexed_block, read_indexed_blockhashes};
use metrics::{CounterVec, HistogramOpts, HistogramVec, MetricOpts, Metrics};
use store::{ReadStore, Row};
use util::{spawn_thread, HeaderList, SyncChannel};

use errors::*;

/// An efficient parser for Bitcoin blk*.dat files.

fn load_headers(daemon: &Daemon) -> Result<HeaderList> {
    let tip = daemon.getbestblockhash()?;
    let mut headers = HeaderList::empty();
    let new_headers = headers.order(daemon.get_new_headers(&headers, &tip)?);
    headers.apply(new_headers);
    Ok(headers)
}

pub struct Parser {
    files: Vec<PathBuf>,
    indexed_blockhashes: HashSet<Sha256dHash>,
    current_headers: HeaderList,
    // metrics
    duration: HistogramVec,
    block_count: CounterVec,
}

impl Parser {
    pub fn new(daemon: &Daemon, store: &ReadStore, metrics: &Metrics) -> Result<Parser> {
        Ok(Parser {
            files: daemon.list_blk_files()?,
            indexed_blockhashes: read_indexed_blockhashes(store),
            current_headers: load_headers(daemon)?,
            duration: metrics.histogram_vec(
                HistogramOpts::new("parse_duration", "Block parsing duration (in seconds)"),
                &["step"],
            ),
            block_count: metrics.counter_vec(
                MetricOpts::new("parse_blocks", "# of block parsed (from blk*.dat)"),
                &["type"],
            ),
        })
    }

    pub fn index_blocks(&mut self, blocks: &[Block]) -> Vec<Vec<Row>> {
        let mut rows = vec![];
        for block in blocks {
            let blockhash = block.bitcoin_hash();
            if self.indexed_blockhashes.contains(&blockhash) {
                self.block_count.with_label_values(&["skipped"]).inc();
                continue;
            }
            if let Some(header) = self.current_headers.header_by_blockhash(&blockhash) {
                let timer = self.duration.with_label_values(&["index"]).start_timer();
                rows.push(index_block(block, header.height()));
                timer.observe_duration();
                self.indexed_blockhashes.insert(blockhash);
                self.block_count.with_label_values(&["indexed"]).inc();
            } else {
                warn!("unknown block {}", blockhash);
                self.block_count.with_label_values(&["unknown"]).inc();
            }
        }
        rows
    }

    pub fn start(mut self) -> Receiver<Result<Vec<Vec<Row>>>> {
        let chan = SyncChannel::new(1);
        let tx = chan.sender();
        let parser = parse_files(self.files.split_off(0), self.duration.clone());
        spawn_thread("bulk_indexer", move || {
            for blocks in parser.iter() {
                let rows = blocks.map(|b| self.index_blocks(&b));
                tx.send(rows).unwrap();
            }
            info!("indexed {} blocks", self.indexed_blockhashes.len());
            if let Some(last_header) = self.current_headers
                .iter()
                .take_while(|h| self.indexed_blockhashes.contains(h.hash()))
                .last()
            {
                let rows = vec![last_indexed_block(last_header.hash())];
                tx.send(Ok(vec![rows])).unwrap();
            }
        });
        chan.into_receiver()
    }
}

fn parse_files(files: Vec<PathBuf>, duration: HistogramVec) -> Receiver<Result<Vec<Block>>> {
    let chan = SyncChannel::new(1);
    let tx = chan.sender();
    let blobs = read_files(files, duration.clone());
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
        let pos = cursor.position();
        let mut decoder = RawDecoder::new(cursor);
        match decoder.read_u32().chain_err(|| "no magic")? {
            0 => {
                cursor = decoder.into_inner(); // skip zeroes
                continue;
            }
            0xD9B4BEF9 => (),
            x => bail!("incorrect magic {:x} at {}", x, pos),
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
