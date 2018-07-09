use bitcoin::blockdata::block::Block;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::SimpleDecoder;
use bitcoin::network::serialize::{deserialize, RawDecoder};
use bitcoin::util::hash::Sha256dHash;
use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Seek, SeekFrom};
use std::path::{Path, PathBuf};
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
    magic: u32,
    files: Vec<PathBuf>,
    indexed_blockhashes: HashSet<Sha256dHash>,
    current_headers: HeaderList,
    // metrics
    duration: HistogramVec,
    block_count: CounterVec,
}

pub struct Rows {
    // A list of rows (for each block at specific blk*.dat file)
    rows: Result<Vec<Vec<Row>>>,
    path: PathBuf,
}

impl Rows {
    pub fn rows(&self) -> &Result<Vec<Vec<Row>>> {
        &self.rows
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Parser {
    pub fn new(daemon: &Daemon, store: &ReadStore, metrics: &Metrics) -> Result<Parser> {
        Ok(Parser {
            magic: daemon.magic(),
            files: daemon.list_blk_files()?,
            indexed_blockhashes: read_indexed_blockhashes(store),
            current_headers: load_headers(daemon)?,
            duration: metrics.histogram_vec(
                HistogramOpts::new("parse_duration", "blk*.dat parsing duration (in seconds)"),
                &["step"],
            ),
            block_count: metrics.counter_vec(
                MetricOpts::new("parse_blocks", "# of block parsed (from blk*.dat)"),
                &["type"],
            ),
        })
    }

    fn index_blocks(&mut self, blocks: &[Block]) -> Vec<Vec<Row>> {
        let mut rows = vec![];
        for block in blocks {
            let blockhash = block.bitcoin_hash();
            if self.indexed_blockhashes.contains(&blockhash) {
                self.block_count.with_label_values(&["skipped"]).inc();
                continue;
            }
            if let Some(header) = self.current_headers.header_by_blockhash(&blockhash) {
                rows.push(index_block(block, header.height()));
                self.indexed_blockhashes.insert(blockhash);
                self.block_count.with_label_values(&["indexed"]).inc();
            } else {
                warn!("unknown block {}", blockhash);
                self.block_count.with_label_values(&["unknown"]).inc();
            }
        }
        rows
    }

    pub fn start(mut self) -> Receiver<Rows> {
        info!("indexing {} blk*.dat files", self.files.len());
        debug!(
            "{} blocks are already indexed",
            self.indexed_blockhashes.len()
        );
        let chan = SyncChannel::new(0);
        let tx = chan.sender();
        let parser = parse_files(self.files.split_off(0), self.duration.clone(), self.magic);
        spawn_thread("bulk_indexer", move || {
            for msg in parser.iter() {
                let timer = self.duration.with_label_values(&["index"]).start_timer();
                let rows = msg.blocks.map(|b| self.index_blocks(&b));
                timer.observe_duration();
                tx.send(Rows {
                    rows,
                    path: msg.path,
                }).unwrap();
            }
            info!("indexed {} blocks", self.indexed_blockhashes.len());
            if let Some(last_header) = self.current_headers
                .iter()
                .take_while(|h| self.indexed_blockhashes.contains(h.hash()))
                .last()
            {
                let rows = Ok(vec![vec![last_indexed_block(last_header.hash())]]);
                tx.send(Rows {
                    rows,
                    path: PathBuf::new(),
                }).unwrap();
            }
        });
        chan.into_receiver()
    }
}

struct Blocks {
    blocks: Result<Vec<Block>>,
    path: PathBuf,
}

fn parse_files(files: Vec<PathBuf>, duration: HistogramVec, magic: u32) -> Receiver<Blocks> {
    let chan = SyncChannel::new(0);
    let tx = chan.sender();
    let blkfiles = read_files(files, duration.clone());
    spawn_thread("bulk_parser", move || {
        for msg in blkfiles.iter() {
            let blocks = match msg.contents {
                Ok(contents) => {
                    let timer = duration.with_label_values(&["parse"]).start_timer();
                    let blocks = parse_blocks(&contents, magic);
                    timer.observe_duration();
                    blocks
                }
                Err(err) => Err(err),
            };
            tx.send(Blocks {
                blocks,
                path: msg.path,
            }).unwrap();
        }
    });
    chan.into_receiver()
}

struct BlkFile {
    contents: Result<Vec<u8>>,
    path: PathBuf,
}

fn read_files(files: Vec<PathBuf>, duration: HistogramVec) -> Receiver<BlkFile> {
    let chan = SyncChannel::new(0);
    let tx = chan.sender();
    spawn_thread("bulk_reader", move || {
        for path in files {
            let timer = duration.with_label_values(&["read"]).start_timer();
            let contents = fs::read(&path).chain_err(|| format!("failed to read {:?}", path));
            timer.observe_duration();
            if let Ok(ref blob) = contents {
                trace!("read {:.2} MB from {:?}", blob.len() as f32 / 1e6, path);
            }
            tx.send(BlkFile { contents, path }).unwrap();
        }
    });
    chan.into_receiver()
}

fn parse_blocks(blob: &[u8], magic: u32) -> Result<Vec<Block>> {
    let mut cursor = Cursor::new(&blob);
    let mut blocks = vec![];
    let max_pos = blob.len() as u64;
    while cursor.position() < max_pos {
        let pos = cursor.position();
        let mut decoder = RawDecoder::new(cursor);
        match decoder.read_u32() {
            Ok(0) => {
                cursor = decoder.into_inner(); // skip zeroes
                continue;
            }
            Ok(x) => {
                if x != magic {
                    bail!("incorrect magic {:08x} at {}", x, pos)
                }
            }
            Err(_) => break, // EOF
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
