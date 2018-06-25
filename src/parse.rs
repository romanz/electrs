use bitcoin::blockdata::block::Block;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::SimpleDecoder;
use bitcoin::network::serialize::{deserialize, RawDecoder};
use std::fs;
use std::io::{Cursor, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use daemon::Daemon;
use index::{index_block, last_indexed_block, read_indexed_blockhashes};
use metrics::{HistogramOpts, HistogramVec, Metrics};
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

pub fn parser(
    daemon: &Daemon,
    store: &ReadStore,
    metrics: &Metrics,
) -> Result<Receiver<Result<Vec<Vec<Row>>>>> {
    let duration = metrics.histogram_vec(
        HistogramOpts::new("parse_duration", "Block parsing duration (in seconds)"),
        &["step"],
    );
    let chan = SyncChannel::new(1);
    let tx = chan.sender();
    let current_headers = load_headers(daemon)?;
    let mut indexed_blockhashes = read_indexed_blockhashes(store);
    info!("loaded {} blockhashes", indexed_blockhashes.len());
    let parser = parse_files(daemon.list_blk_files()?, duration.clone());
    spawn_thread("bulk_indexer", move || {
        for msg in parser.iter() {
            match msg {
                Ok(blocks) => {
                    let mut rows = vec![];
                    for block in &blocks {
                        let blockhash = block.bitcoin_hash();
                        if indexed_blockhashes.contains(&blockhash) {
                            continue;
                        }
                        if let Some(header) = current_headers.header_by_blockhash(&blockhash) {
                            let timer = duration.with_label_values(&["index"]).start_timer();
                            rows.push(index_block(block, header.height()));
                            timer.observe_duration();
                            indexed_blockhashes.insert(blockhash);
                        } else {
                            warn!("unknown block {}", blockhash);
                        }
                    }
                    tx.send(Ok(rows)).unwrap();
                }
                Err(err) => {
                    tx.send(Err(err)).unwrap();
                }
            }
        }
        info!("indexed {} blocks", indexed_blockhashes.len());
        for header in current_headers.iter().rev() {
            if indexed_blockhashes.contains(header.hash()) {
                info!("{:?}", header);
                let rows = vec![last_indexed_block(header.hash())];
                tx.send(Ok(vec![rows])).unwrap();
                return;
            }
        }
    });
    Ok(chan.into_receiver())
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
