use bitcoin::blockdata::block::Block;
use bitcoin::consensus::encode::{deserialize, Decodable};
use bitcoin::util::hash::{BitcoinHash, Sha256dHash};
use rayon::prelude::*;

use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::thread;

use crate::daemon::Daemon;
use crate::errors::*;
use crate::util::{spawn_thread, HeaderEntry, SyncChannel};

pub struct BlockEntry {
    pub block: Block,
    pub entry: HeaderEntry,
}

pub struct Fetcher<T> {
    receiver: Receiver<T>,
    thread: thread::JoinHandle<()>,
}

impl<T> Fetcher<T> {
    fn from(receiver: Receiver<T>, thread: thread::JoinHandle<()>) -> Self {
        Fetcher { receiver, thread }
    }

    pub fn map<F>(self, mut func: F)
    where
        F: FnMut(T) -> (),
    {
        for item in self.receiver {
            func(item);
        }
        self.thread.join().expect("fetcher thread panicked")
    }
}

pub fn bitcoind_fetcher(
    daemon: &Daemon,
    new_headers: &[HeaderEntry],
) -> Result<Fetcher<Vec<BlockEntry>>> {
    new_headers.last().map(|tip| {
        info!("{:?} ({} new headers)", tip, new_headers.len());
    });
    let new_headers = new_headers.to_vec();
    let daemon = daemon.reconnect()?;
    let chan = SyncChannel::new(1);
    let sender = chan.sender();
    Ok(Fetcher::from(
        chan.into_receiver(),
        spawn_thread("bitcoind_fetcher", move || -> () {
            for entries in new_headers.chunks(100) {
                let blockhashes: Vec<Sha256dHash> = entries.iter().map(|e| *e.hash()).collect();
                let blocks = daemon
                    .getblocks(&blockhashes)
                    .expect("failed to get blocks from bitcoind");
                assert_eq!(blocks.len(), entries.len());
                let block_entries: Vec<BlockEntry> = blocks
                    .into_iter()
                    .zip(entries)
                    .map(|(block, entry)| BlockEntry {
                        block,
                        entry: entry.clone(), // TODO: remove this clone()
                    })
                    .collect();
                assert_eq!(block_entries.len(), entries.len());
                sender
                    .send(block_entries)
                    .expect("failed to send fetched blocks");
            }
        }),
    ))
}

pub fn blkfiles_fetcher(
    daemon: &Daemon,
    new_headers: &[HeaderEntry],
) -> Result<Fetcher<Vec<BlockEntry>>> {
    let magic = daemon.magic();
    let blk_files = daemon.list_blk_files()?;

    let chan = SyncChannel::new(1);
    let sender = chan.sender();

    let mut entry_map: HashMap<Sha256dHash, HeaderEntry> =
        new_headers.iter().map(|h| (*h.hash(), h.clone())).collect();

    let parser = blkfiles_parser(blkfiles_reader(blk_files), magic);
    Ok(Fetcher::from(
        chan.into_receiver(),
        spawn_thread("bitcoind_fetcher", move || -> () {
            parser.map(|blocks| {
                let block_entries: Vec<BlockEntry> = blocks
                    .into_iter()
                    .filter_map(|block| {
                        let blockhash = block.bitcoin_hash();
                        entry_map
                            .remove(&blockhash)
                            .map(|entry| BlockEntry { block, entry })
                            .or_else(|| {
                                debug!("unknown block {}", blockhash);
                                None
                            })
                    })
                    .collect();
                trace!("fetched {} blocks", block_entries.len());
                sender
                    .send(block_entries)
                    .expect("failed to send blocks entries from blk*.dat files");
            });
            if !entry_map.is_empty() {
                panic!(
                    "failed to index {} blocks from blk*.dat files",
                    entry_map.len()
                )
            }
        }),
    ))
}

fn blkfiles_reader(blk_files: Vec<PathBuf>) -> Fetcher<Vec<u8>> {
    let chan = SyncChannel::new(1);
    let sender = chan.sender();

    Fetcher::from(
        chan.into_receiver(),
        spawn_thread("blkfiles_reader", move || -> () {
            for path in blk_files {
                trace!("reading {:?}", path);
                let blob = fs::read(&path).expect(&format!("failed to read {:?}", path));
                sender
                    .send(blob)
                    .expect(&format!("failed to send {:?} contents", path));
            }
        }),
    )
}

fn blkfiles_parser(blobs: Fetcher<Vec<u8>>, magic: u32) -> Fetcher<Vec<Block>> {
    let chan = SyncChannel::new(1);
    let sender = chan.sender();

    Fetcher::from(
        chan.into_receiver(),
        spawn_thread("blkfiles_parser", move || -> () {
            blobs.map(|blob| {
                trace!("parsing {} bytes", blob.len());
                let blocks = parse_blocks(blob, magic).expect("failed to parse blk*.dat file");
                sender
                    .send(blocks)
                    .expect("failed to send blocks from blk*.dat file");
            });
        }),
    )
}

fn parse_blocks(blob: Vec<u8>, magic: u32) -> Result<Vec<Block>> {
    let mut cursor = Cursor::new(&blob);
    let mut slices = vec![];
    let max_pos = blob.len() as u64;
    while cursor.position() < max_pos {
        match u32::consensus_decode(&mut cursor) {
            Ok(value) => {
                if magic != value {
                    cursor
                        .seek(SeekFrom::Current(-3))
                        .expect("failed to seek back");
                    continue;
                }
            }
            Err(_) => break, // EOF
        };
        let block_size = u32::consensus_decode(&mut cursor).chain_err(|| "no block size")?;
        let start = cursor.position() as usize;
        cursor
            .seek(SeekFrom::Current(block_size as i64))
            .chain_err(|| format!("seek {} failed", block_size))?;
        let end = cursor.position() as usize;

        slices.push(&blob[start..end])
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(0) // CPU-bound
        .thread_name(|i| format!("parse-blocks-{}", i))
        .build()
        .unwrap();
    Ok(pool.install(|| {
        slices
            .par_iter()
            .map(|slice| deserialize(slice).expect("failed to parse Block"))
            .collect()
    }))
}
