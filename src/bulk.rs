use bitcoin::blockdata::block::Block;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::SimpleDecoder;
use bitcoin::network::serialize::{deserialize, RawDecoder};
use bitcoin::util::hash::Sha256dHash;
use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use daemon::Daemon;
use index::{index_block, last_indexed_block};
use metrics::{CounterVec, Histogram, HistogramOpts, HistogramVec, MetricOpts, Metrics};
use store::Row;
use util::HeaderList;

use errors::*;

/// An efficient parser for Bitcoin blk*.dat files.

pub struct Parser {
    magic: u32,
    current_headers: HeaderList,
    indexed_blockhashes: Mutex<HashSet<Sha256dHash>>,
    // metrics
    duration: HistogramVec,
    block_count: CounterVec,
    bytes_read: Histogram,
}

impl Parser {
    pub fn new(daemon: &Daemon, metrics: &Metrics) -> Result<Parser> {
        Ok(Parser {
            magic: daemon.magic(),
            current_headers: load_headers(daemon)?,
            indexed_blockhashes: Mutex::new(HashSet::new()),
            duration: metrics.histogram_vec(
                HistogramOpts::new("parse_duration", "blk*.dat parsing duration (in seconds)"),
                &["step"],
            ),
            block_count: metrics.counter_vec(
                MetricOpts::new("parse_blocks", "# of block parsed (from blk*.dat)"),
                &["type"],
            ),

            bytes_read: metrics.histogram(HistogramOpts::new(
                "parse_bytes_read",
                "# of bytes read (from blk*.dat)",
            )),
        })
    }

    pub fn read_blkfile(&self, path: &Path) -> Result<Vec<u8>> {
        let timer = self.duration.with_label_values(&["read"]).start_timer();
        let blob = fs::read(&path).chain_err(|| format!("failed to read {:?}", path))?;
        timer.observe_duration();
        self.bytes_read.observe(blob.len() as f64);
        return Ok(blob);
    }

    pub fn index_blkfile(&self, blob: Vec<u8>) -> Result<Vec<Row>> {
        let timer = self.duration.with_label_values(&["parse"]).start_timer();
        let blocks = parse_blocks(blob, self.magic)?;
        timer.observe_duration();

        let mut rows = Vec::<Row>::new();
        let timer = self.duration.with_label_values(&["index"]).start_timer();
        for block in blocks {
            let blockhash = block.bitcoin_hash();
            if let Some(header) = self.current_headers.header_by_blockhash(&blockhash) {
                if self.indexed_blockhashes
                    .lock()
                    .expect("indexed_blockhashes")
                    .insert(blockhash.clone())
                {
                    rows.extend(index_block(&block, header.height()));
                    self.block_count.with_label_values(&["indexed"]).inc();
                } else {
                    self.block_count.with_label_values(&["duplicate"]).inc();
                }
            } else {
                debug!("skipping block {}", blockhash); // will be indexed later (after bulk load is over)
                self.block_count.with_label_values(&["skipped"]).inc();
            }
        }
        timer.observe_duration();

        let timer = self.duration.with_label_values(&["sort"]).start_timer();
        rows.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        timer.observe_duration();
        Ok(rows)
    }
}

fn parse_blocks(blob: Vec<u8>, magic: u32) -> Result<Vec<Block>> {
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
    Ok(blocks)
}

fn load_headers(daemon: &Daemon) -> Result<HeaderList> {
    let tip = daemon.getbestblockhash()?;
    let mut headers = HeaderList::empty();
    let new_headers = headers.order(daemon.get_new_headers(&headers, &tip)?);
    headers.apply(new_headers);
    Ok(headers)
}
