use rocksdb;
use rocksdb::Writable;

use std::path::Path;

use util::Bytes;

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
    fn write(&self, rows_vec: Vec<Vec<Row>>);
    fn flush(&self);
}

pub struct DBStore {
    db: rocksdb::DB,
    opts: StoreOptions,
}

#[derive(Debug)]
pub struct StoreOptions {
    pub bulk_import: bool,
}

impl DBStore {
    /// Opens a new RocksDB at the specified location.
    pub fn open(path: &Path, opts: StoreOptions) -> DBStore {
        let path = path.to_str().unwrap();
        debug!("opening {:?} with {:?}", path, &opts);
        let mut db_opts = rocksdb::DBOptions::default();
        db_opts.create_if_missing(true);
        db_opts.increase_parallelism(2);

        let mut cf_opts = rocksdb::rocksdb_options::ColumnFamilyOptions::new();
        cf_opts.set_compaction_style(rocksdb::DBCompactionStyle::Level);
        cf_opts.compression(rocksdb::DBCompressionType::Snappy);
        cf_opts.set_target_file_size_base(128 << 20);
        cf_opts.set_write_buffer_size(64 << 20);
        cf_opts.set_min_write_buffer_number(2);
        cf_opts.set_max_write_buffer_number(3);
        cf_opts.set_disable_auto_compactions(opts.bulk_import);

        let mut block_opts = rocksdb::BlockBasedOptions::default();
        block_opts.set_block_size(256 << 10);
        DBStore {
            db: rocksdb::DB::open_cf(db_opts, path, vec![("default", cf_opts)]).unwrap(),
            opts: opts,
        }
    }

    pub fn put(&self, key: &[u8], value: &[u8]) {
        self.db.put(key, value).unwrap();
    }

    pub fn compact(&self) {
        info!("starting full compaction");
        self.db.compact_range(None, None); // would take a while
        info!("finished full compaction");
    }
}

impl ReadStore for DBStore {
    fn get(&self, key: &[u8]) -> Option<Bytes> {
        self.db.get(key).unwrap().map(|v| v.to_vec())
    }

    // TODO: use generators
    fn scan(&self, prefix: &[u8]) -> Vec<Row> {
        let mut rows = vec![];
        let mut iter = self.db.iter();
        iter.seek(rocksdb::SeekKey::Key(prefix));
        for (key, value) in &mut iter {
            if !key.starts_with(prefix) {
                break;
            }
            rows.push(Row { key, value });
        }
        rows
    }
}

impl WriteStore for DBStore {
    fn write(&self, rows_vec: Vec<Vec<Row>>) {
        let batch = rocksdb::WriteBatch::default();
        for rows in rows_vec {
            for row in rows {
                batch.put(row.key.as_slice(), row.value.as_slice()).unwrap();
            }
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
        trace!("closing DB");
    }
}
