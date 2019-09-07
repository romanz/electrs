use rocksdb;
use std::path::{Path, PathBuf};

use crate::util::Bytes;

#[derive(Clone)]
pub struct Row {
    pub key: Bytes,
    pub value: Bytes,
}

impl Row {
    pub fn into_pair(self) -> (Bytes, Bytes) {
        (self.key, self.value)
    }
}

pub trait ReadStore: Sync {
    fn get(&self, key: &[u8]) -> Option<Bytes>;
    fn scan(&self, prefix: &[u8]) -> Vec<Row>;
}

pub trait WriteStore: Sync {
    fn write<I: IntoIterator<Item = Row>>(&self, rows: I);
    fn flush(&self);
}

#[derive(Clone)]
struct Options {
    path: PathBuf,
    bulk_import: bool,
    low_memory: bool,
}

pub struct DBStore {
    db: rocksdb::DB,
    opts: Options,
}

impl DBStore {
    fn open_opts(opts: Options) -> Self {
        debug!("opening DB at {:?}", opts.path);
        let mut db_opts = rocksdb::Options::default();
        db_opts.create_if_missing(true);
        // db_opts.set_keep_log_file_num(10);
        db_opts.set_max_open_files(if opts.bulk_import { 16 } else { 256 });
        db_opts.set_compaction_style(rocksdb::DBCompactionStyle::Level);
        db_opts.set_compression_type(rocksdb::DBCompressionType::Snappy);
        db_opts.set_target_file_size_base(256 << 20);
        db_opts.set_write_buffer_size(256 << 20);
        db_opts.set_disable_auto_compactions(opts.bulk_import); // for initial bulk load
        db_opts.set_advise_random_on_open(!opts.bulk_import); // bulk load uses sequential I/O
        if !opts.low_memory {
            db_opts.set_compaction_readahead_size(1 << 20);
        }

        let mut block_opts = rocksdb::BlockBasedOptions::default();
        block_opts.set_block_size(if opts.low_memory { 256 << 10 } else { 1 << 20 });
        DBStore {
            db: rocksdb::DB::open(&db_opts, &opts.path).unwrap(),
            opts,
        }
    }

    /// Opens a new RocksDB at the specified location.
    pub fn open(path: &Path, low_memory: bool) -> Self {
        DBStore::open_opts(Options {
            path: path.to_path_buf(),
            bulk_import: true,
            low_memory,
        })
    }

    pub fn enable_compaction(self) -> Self {
        let mut opts = self.opts.clone();
        if opts.bulk_import {
            opts.bulk_import = false;
            info!("enabling auto-compactions");
            let opts = [("disable_auto_compactions", "false")];
            self.db.set_options(&opts).unwrap();
        }
        self
    }

    pub fn compact(self) -> Self {
        info!("starting full compaction");
        self.db.compact_range(None::<&[u8]>, None::<&[u8]>); // would take a while
        info!("finished full compaction");
        self
    }

    pub fn iter_scan(&self, prefix: &[u8]) -> ScanIterator {
        ScanIterator {
            prefix: prefix.to_vec(),
            iter: self.db.prefix_iterator(prefix),
            done: false,
        }
    }
}

pub struct ScanIterator<'a> {
    prefix: Vec<u8>,
    iter: rocksdb::DBIterator<'a>,
    done: bool,
}

impl<'a> Iterator for ScanIterator<'a> {
    type Item = Row;

    fn next(&mut self) -> Option<Row> {
        if self.done {
            return None;
        }
        let (key, value) = self.iter.next()?;
        if !key.starts_with(&self.prefix) {
            self.done = true;
            return None;
        }
        Some(Row {
            key: key.to_vec(),
            value: value.to_vec(),
        })
    }
}

impl ReadStore for DBStore {
    fn get(&self, key: &[u8]) -> Option<Bytes> {
        self.db.get(key).unwrap().map(|v| v.to_vec())
    }

    // TODO: use generators
    fn scan(&self, prefix: &[u8]) -> Vec<Row> {
        let mut rows = vec![];
        for (key, value) in self.db.iterator(rocksdb::IteratorMode::From(
            prefix,
            rocksdb::Direction::Forward,
        )) {
            if !key.starts_with(prefix) {
                break;
            }
            rows.push(Row {
                key: key.to_vec(),
                value: value.to_vec(),
            });
        }
        rows
    }
}

impl WriteStore for DBStore {
    fn write<I: IntoIterator<Item = Row>>(&self, rows: I) {
        let mut batch = rocksdb::WriteBatch::default();
        for row in rows {
            batch.put(row.key.as_slice(), row.value.as_slice()).unwrap();
        }
        let mut opts = rocksdb::WriteOptions::new();
        opts.set_sync(!self.opts.bulk_import);
        opts.disable_wal(self.opts.bulk_import);
        self.db.write_opt(batch, &opts).unwrap();
    }

    fn flush(&self) {
        let mut opts = rocksdb::WriteOptions::new();
        opts.set_sync(true);
        opts.disable_wal(false);
        let empty = rocksdb::WriteBatch::default();
        self.db.write_opt(empty, &opts).unwrap();
    }
}

impl Drop for DBStore {
    fn drop(&mut self) {
        trace!("closing DB at {:?}", self.opts.path);
    }
}

fn full_compaction_marker() -> Row {
    Row {
        key: b"F".to_vec(),
        value: b"".to_vec(),
    }
}

pub fn full_compaction(store: DBStore) -> DBStore {
    store.flush();
    let store = store.compact().enable_compaction();
    store.write(vec![full_compaction_marker()]);
    store
}

pub fn is_fully_compacted(store: &dyn ReadStore) -> bool {
    let marker = store.get(&full_compaction_marker().key);
    marker.is_some()
}
