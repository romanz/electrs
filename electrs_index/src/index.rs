use anyhow::{Context, Result};

use bitcoin::{
    consensus::{serialize, Decodable},
    hashes::hex::ToHex,
    Block, BlockHash, BlockHeader, Txid, VarInt,
};

use std::{
    collections::BTreeMap,
    io::{Cursor, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc, Mutex, RwLock, RwLockReadGuard,
    },
    time::Instant,
};

use crate::{
    daemon::{BlockLocation, Daemon},
    db,
    map::BlockMap,
    metrics::{Histogram, Metrics},
    types::{BlockRow, Confirmed, FilePos, Reader, ScriptHash, ScriptHashRow, TxidRow},
    undo::BlockUndo,
};

pub struct Index {
    store: RwLock<db::DBStore>,
    map: RwLock<BlockMap>,
    stats: Stats,
    low_memory: bool,
}

#[derive(Clone)]
struct Stats {
    update_duration: Histogram,
    update_size: Histogram,
    lookup_duration: Histogram,
}

struct ReadRequest {
    file_id: usize,
    locations: Vec<BlockLocation>,
    blk_file: PathBuf,
    undo_file: PathBuf,
}

struct ParseResult {
    loc: BlockLocation,
    block: Block,
    undo: BlockUndo,
}

struct IndexRequest {
    file_id: usize,
    locations: Vec<BlockLocation>,
    blk_file: Cursor<Vec<u8>>,
    undo_file: Cursor<Vec<u8>>,
}

struct IndexResult {
    header_row: BlockRow,
    index_rows: Vec<ScriptHashRow>,
    txid_rows: Vec<TxidRow>,
}

impl IndexResult {
    fn extend(&self, batch: &mut db::WriteBatch) {
        batch
            .index_rows
            .extend(self.index_rows.iter().map(ScriptHashRow::to_db_row));
        batch
            .txid_rows
            .extend(self.txid_rows.iter().map(TxidRow::to_db_row));
        batch.header_rows.push(self.header_row.to_db_row());
    }
}

pub struct LookupResult {
    pub readers: Vec<ConfirmedReader>,
    pub tip: BlockHash,
}

impl Index {
    pub fn new(store: db::DBStore, metrics: &Metrics, low_memory: bool) -> Result<Self> {
        let update_duration = metrics.histogram_vec(
            "index_update_duration",
            "Index duration (in seconds)",
            &["step"],
        );
        let update_size =
            metrics.histogram_vec("index_update_size", "Index size (in bytes)", &["step"]);
        let lookup_duration = metrics.histogram_vec(
            "index_lookup_duration",
            "Lookup duration (in seconds)",
            &["step"],
        );

        let start = Instant::now();
        let rows = store.read_headers();
        let size: u64 = rows.iter().map(|row| row.len() as u64).sum();
        let block_rows = rows
            .into_iter()
            .map(|row| BlockRow::from_db_row(&row))
            .collect::<Result<Vec<BlockRow>>>()
            .context("failed to read block rows")?;
        debug!(
            "read {} blocks ({:.3} MB) from DB at {:.3} ms",
            block_rows.len(),
            size as f64 / 1e6,
            start.elapsed().as_millis()
        );
        let map = BlockMap::new(block_rows);

        Ok(Index {
            store: RwLock::new(store),
            map: RwLock::new(map),
            stats: Stats {
                update_duration,
                update_size,
                lookup_duration,
            },
            low_memory,
        })
    }

    pub fn map(&self) -> RwLockReadGuard<BlockMap> {
        self.map.read().unwrap()
    }

    pub fn block_reader(&self, blockhash: &BlockHash, daemon: &Daemon) -> Result<BlockReader> {
        Ok(match self.map().get_block_pos(blockhash) {
            None => bail!("unknown block {}", blockhash),
            Some(pos) => BlockReader {
                reader: pos.reader(daemon),
                lookup_duration: self.stats.lookup_duration.clone(),
            },
        })
    }

    pub fn lookup_by_txid(&self, txid: &Txid, daemon: &Daemon) -> Result<LookupResult> {
        let txid_prefix = TxidRow::scan_prefix(txid);
        let positions: Vec<FilePos> =
            self.stats
                .lookup_duration
                .observe_duration("lookup_scan_rows", || {
                    self.store
                        .read()
                        .unwrap()
                        .iter_txid(&txid_prefix)
                        .map(|row| TxidRow::from_db_row(&row).map(|row| *row.position()))
                        .collect::<Result<Vec<_>>>()
                        .context("failed to parse txid rows")
                })?;
        self.create_readers(txid.to_hex(), positions, daemon)
    }

    pub fn lookup_by_scripthash(
        &self,
        script_hash: &ScriptHash,
        daemon: &Daemon,
    ) -> Result<LookupResult> {
        let scan_prefix = ScriptHashRow::scan_prefix(script_hash);
        let positions: Vec<FilePos> =
            self.stats
                .lookup_duration
                .observe_duration("lookup_scan_rows", || {
                    self.store
                        .read()
                        .unwrap()
                        .iter_index(&scan_prefix)
                        .map(|row| ScriptHashRow::from_db_row(&row).map(|row| *row.position()))
                        .collect::<Result<Vec<_>>>()
                        .context("failed to parse index rows")
                })?;

        self.create_readers(script_hash.to_hex(), positions, daemon)
    }

    fn create_readers(
        &self,
        label: String,
        positions: Vec<FilePos>,
        daemon: &Daemon,
    ) -> Result<LookupResult> {
        debug!("{} has {} rows", label, positions.len());
        let map = self.map(); // lock block map for concurrent updates
        let readers = positions
            .into_iter()
            .filter_map(|pos| match map.find_block(pos) {
                Some((hash, height)) => {
                    let header = map.get_by_hash(hash).expect("missing block header");
                    Some(ConfirmedReader {
                        reader: pos.reader(daemon),
                        lookup_duration: self.stats.lookup_duration.clone(),
                        header: *header,
                        height,
                    })
                }
                None => {
                    warn!("{:?} {} not confirmed", pos, label);
                    None
                }
            })
            .collect();
        let tip = map.chain().last().cloned().unwrap_or_default();
        Ok(LookupResult { readers, tip })
    }

    fn start_reader(
        &self,
        requests: Vec<ReadRequest>,
        reader_tx: SyncSender<IndexRequest>,
    ) -> Result<std::thread::JoinHandle<Result<()>>> {
        let update_duration = self.stats.update_duration.clone();
        let update_size = self.stats.update_size.clone();
        let reader_thread = std::thread::spawn(move || -> Result<()> {
            for r in requests {
                let blk_file =
                    update_duration.observe_duration("load_block", || read_file(&r.blk_file))?;
                update_size.observe_size("load_block", blk_file.len());

                let undo_file =
                    update_duration.observe_duration("load_undo", || read_file(&r.undo_file))?;
                update_size.observe_size("load_undo", undo_file.len());

                let file_id = r.file_id;
                let index_request = IndexRequest {
                    file_id,
                    locations: r.locations,
                    blk_file: Cursor::new(blk_file),
                    undo_file: Cursor::new(undo_file),
                };
                reader_tx
                    .send(index_request)
                    .with_context(|| format!("reader send failed: file_id={}", file_id))?
            }
            Ok(())
        });
        Ok(reader_thread)
    }

    fn start_indexer(
        &self,
        i: usize,
        reader_rx: Arc<Mutex<Receiver<IndexRequest>>>,
        indexer_tx: SyncSender<db::WriteBatch>,
    ) -> std::thread::JoinHandle<Result<()>> {
        let update_duration = self.stats.update_duration.clone();
        std::thread::spawn(move || {
            loop {
                let IndexRequest {
                    file_id,
                    locations,
                    mut blk_file,
                    mut undo_file,
                } = {
                    // make sure receiver is unlocked after recv() is over
                    match reader_rx.lock().unwrap().recv() {
                        Ok(msg) => msg,
                        Err(_) => break, // channel has disconnected
                    }
                };
                let parsed: Vec<ParseResult> = update_duration.observe_duration("parse", || {
                    locations
                        .into_iter()
                        .map(|loc| parse_block_and_undo(loc, &mut blk_file, &mut undo_file))
                        .collect()
                });
                let blocks_count = parsed.len();

                let mut result = db::WriteBatch::default();
                update_duration.observe_duration("index", || {
                    parsed.into_iter().for_each(|p| {
                        index_single_block(p).extend(&mut result);
                    });
                });
                debug!(
                    "indexer #{}: indexed {} blocks from file #{} into {} rows",
                    i,
                    blocks_count,
                    file_id,
                    result.index_rows.len()
                );

                update_duration.observe_duration("sort", || {
                    result.index_rows.sort_unstable();
                    result.header_rows.sort_unstable();
                });

                indexer_tx
                    .send(result)
                    .with_context(|| format!("indexer send failed: file_id={}", file_id))?
            }
            Ok(())
        })
    }

    fn report_stats(&self, batch: &db::WriteBatch) {
        self.stats
            .update_size
            .observe_size("write_index_rows", db_rows_size(&batch.index_rows));
        self.stats
            .update_size
            .observe_size("write_txid_rows", db_rows_size(&batch.txid_rows));
        self.stats
            .update_size
            .observe_size("write_header_rows", db_rows_size(&batch.header_rows));
        debug!(
            "writing {} scripthash rows from {} transactions, {} blocks",
            batch.index_rows.len(),
            batch.txid_rows.len(),
            batch.header_rows.len()
        );
    }

    pub fn update(&self, daemon: &Daemon) -> Result<BlockHash> {
        let start = Instant::now();
        let tip = daemon.get_best_block_hash()?;
        let locations = {
            let map = self.map();
            if map.chain().last() == Some(&tip) {
                debug!("skip update, same tip: {}", tip);
                return Ok(tip);
            }
            load_locations(daemon, tip, &map)?
        };
        let read_requests = group_locations_by_file(locations)
            .into_iter()
            .map(|(file_id, locations)| ReadRequest {
                file_id,
                locations,
                blk_file: daemon.blk_file_path(file_id),
                undo_file: daemon.undo_file_path(file_id),
            })
            .collect::<Vec<_>>();
        let new_blocks = read_requests
            .iter()
            .map(|r| r.locations.len())
            .sum::<usize>();
        info!(
            "reading {} new blocks from {} blk*.dat files",
            new_blocks,
            read_requests.len()
        );

        if self.low_memory {
            let mut block_rows = vec![];
            for r in read_requests {
                debug!("reading {} blocks from {:?}", r.locations.len(), r.blk_file);
                let mut blk_file = std::fs::File::open(r.blk_file)?;
                let mut undo_file = std::fs::File::open(r.undo_file)?;
                let mut batch = db::WriteBatch::default();
                for loc in r.locations {
                    let parsed = parse_block_and_undo(loc, &mut blk_file, &mut undo_file);
                    index_single_block(parsed).extend(&mut batch);
                }
                let batch_block_rows = batch
                    .header_rows
                    .iter()
                    .map(|row| BlockRow::from_db_row(row).expect("bad BlockRow"));
                block_rows.extend(batch_block_rows);
                self.stats
                    .update_duration
                    .observe_duration("write", || self.store.read().unwrap().write(batch));
            }
            self.store.write().unwrap().flush();
            return self.map.write().unwrap().update_chain(block_rows, tip);
        }

        let (reader_tx, reader_rx) = sync_channel(0);
        let (indexer_tx, indexer_rx) = sync_channel(0);

        let reader_thread = self.start_reader(read_requests, reader_tx)?;

        let reader_rx = Arc::new(Mutex::new(reader_rx));
        let indexers: Vec<std::thread::JoinHandle<_>> = (0..4)
            .map(|i| self.start_indexer(i, Arc::clone(&reader_rx), indexer_tx.clone()))
            .collect();
        drop(indexer_tx); // no need for the original sender

        let mut block_rows = vec![];
        let mut total_rows_count = 0usize;
        for batch in indexer_rx.into_iter() {
            let batch_block_rows = batch
                .header_rows
                .iter()
                .map(|row| BlockRow::from_db_row(row).expect("bad BlockRow"));
            block_rows.extend(batch_block_rows);

            self.report_stats(&batch);
            total_rows_count += self
                .stats
                .update_duration
                .observe_duration("write", || self.store.read().unwrap().write(batch));
        }

        indexers
            .into_iter()
            .map(|indexer| indexer.join().expect("indexer thread panicked"))
            .collect::<Result<_>>()
            .context("indexer thread failed")?;

        reader_thread
            .join()
            .expect("reader thread panicked")
            .context("reader thread failed")?;

        info!(
            "indexed {} new blocks, {} DB rows, took {:.3}s",
            block_rows.len(),
            total_rows_count,
            start.elapsed().as_millis() as f64 / 1e3
        );
        assert_eq!(new_blocks, block_rows.len());

        // allow only one thread to apply full compaction
        self.store.write().unwrap().flush();
        self.map.write().unwrap().update_chain(block_rows, tip)
    }
}

fn db_rows_size(rows: &[db::Row]) -> usize {
    rows.iter().map(|key| key.len()).sum()
}

fn index_single_block(parsed: ParseResult) -> IndexResult {
    let ParseResult { loc, block, undo } = parsed;
    assert!(undo.txdata.len() + 1 == block.txdata.len());
    let mut index_rows = vec![];
    let mut txid_rows = Vec::with_capacity(block.txdata.len());

    let file_id = loc.file as u16;
    let txcount = VarInt(block.txdata.len() as u64);
    let mut next_tx_offset: usize =
        loc.data + serialize(&block.header).len() + serialize(&txcount).len();
    let mut undo_iter = undo.txdata.into_iter();

    for tx in &block.txdata {
        let tx_offset = next_tx_offset as u32;
        let pos = FilePos {
            file_id,
            offset: tx_offset,
        };
        txid_rows.push(TxidRow::new(tx.txid(), pos));

        next_tx_offset += tx.get_size();

        let create_index_row = |script| {
            let pos = FilePos {
                file_id,
                offset: tx_offset,
            };
            ScriptHashRow::new(ScriptHash::new(script), pos)
        };
        index_rows.extend(
            tx.output
                .iter()
                .map(|txo| create_index_row(&txo.script_pubkey)),
        );

        if tx.is_coin_base() {
            continue; // coinbase doesn't have an undo
        }
        let txundo = undo_iter.next().expect("no txundo");
        assert_eq!(tx.input.len(), txundo.scripts.len());
        index_rows.extend(txundo.scripts.iter().map(|script| create_index_row(script)));
    }
    assert!(undo_iter.next().is_none());
    let block_pos = FilePos {
        file_id,
        offset: loc.data as u32,
    };
    let block_size = (next_tx_offset - loc.data) as u32;
    IndexResult {
        index_rows,
        txid_rows,
        header_row: BlockRow::new(block.header, block_pos, block_size),
    }
}

fn load_locations(
    daemon: &Daemon,
    mut blockhash: BlockHash,
    existing: &BlockMap,
) -> Result<Vec<BlockLocation>> {
    let start = Instant::now();
    let null = BlockHash::default();
    let mut result = vec![];

    let mut loc_chunk = Vec::<BlockLocation>::new();
    let mut loc_iter = loc_chunk.into_iter();

    let mut total_blocks = 0usize;
    let mut chunk_size = 10;
    // scan until genesis
    while blockhash != null {
        if existing.in_valid_chain(&blockhash) {
            break; // no need to continue validation
        }
        total_blocks += 1;
        if let Some(header) = existing.get_by_hash(&blockhash) {
            // this block was indexed - make sure it points back correctly (advancing loc_iter)
            if let Some(loc) = loc_iter.next() {
                assert_eq!(loc.prev, header.prev_blockhash);
            }
            blockhash = header.prev_blockhash;
            continue;
        }
        // get next block location
        let location = match loc_iter.next() {
            // get a new chunk from daemon if needed
            Some(loc) => loc,
            None => {
                loc_chunk = daemon.get_block_locations(&blockhash, chunk_size)?;
                chunk_size = std::cmp::min(chunk_size * 10, 100_000); // takes <1s
                trace!("got {} block locations", loc_chunk.len());
                loc_iter = loc_chunk.into_iter();
                loc_iter.next().unwrap()
            }
        };
        blockhash = location.prev;
        result.push(location);
    }
    debug!(
        "loaded {} block locations ({} new) at {} ms",
        total_blocks,
        result.len(),
        start.elapsed().as_millis(),
    );
    Ok(result)
}

fn group_locations_by_file(locations: Vec<BlockLocation>) -> BTreeMap<usize, Vec<BlockLocation>> {
    let mut locations_by_file = BTreeMap::<usize, Vec<BlockLocation>>::new();
    locations
        .into_iter()
        .for_each(|loc| locations_by_file.entry(loc.file).or_default().push(loc));
    locations_by_file
        .values_mut()
        .for_each(|locations| locations.sort_by_key(|loc| loc.data));
    locations_by_file
}

fn parse_from<T: Decodable, F: Read + Seek>(src: &mut F, offset: usize) -> Result<T> {
    src.seek(SeekFrom::Start(offset as u64))?;
    Ok(Decodable::consensus_decode(src).context("parsing failed")?)
}

fn parse_block_and_undo<F>(loc: BlockLocation, blk_file: &mut F, undo_file: &mut F) -> ParseResult
where
    F: Read + Seek,
{
    let block: Block = match parse_from(blk_file, loc.data) {
        Ok(block) => block,
        Err(err) => panic!("bad Block at {:?}: {}", loc, err),
    };
    let undo: BlockUndo = match loc.undo.map(|offset| parse_from(undo_file, offset)) {
        Some(Ok(undo)) => undo,
        Some(Err(err)) => panic!("bad BlockUndo at {:?}: {}", loc, err),
        None => {
            // genesis block has no undo
            assert_eq!(block.header.prev_blockhash, BlockHash::default());
            BlockUndo::default() // create an empty undo
        }
    };
    ParseResult { loc, block, undo }
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    Ok(std::fs::read(&path).with_context(|| format!("failed to read {:?}", path))?)
}

pub struct ConfirmedReader {
    reader: Reader,
    lookup_duration: Histogram,
    header: BlockHeader,
    height: usize,
}

impl ConfirmedReader {
    pub fn read(self) -> Result<Confirmed> {
        let tx = self
            .lookup_duration
            .observe_duration("lookup_read_tx", || {
                self.reader
                    .read()
                    .context("failed to read confirmed transaction")
            })?;

        Ok(Confirmed::new(tx, self.header, self.height, &self.reader))
    }
}

pub struct BlockReader {
    reader: Reader,
    lookup_duration: Histogram,
}

impl BlockReader {
    pub fn read(self) -> Result<Block> {
        self.lookup_duration
            .observe_duration("lookup_read_block", || {
                self.reader.read().context("failed to read block")
            })
    }
}
