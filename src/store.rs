use rocksdb;
use std::sync::mpsc::Receiver;

use signal::Waiter;
use util::Bytes;

use errors::*;

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
    path: String,
    opts: StoreOptions,
}

#[derive(Debug)]
pub struct StoreOptions {
    pub bulk_import: bool,
}

impl DBStore {
    /// Opens a new RocksDB at the specified location.
    pub fn open(path: &str, opts: StoreOptions) -> DBStore {
        debug!("opening {} with {:?}", path, &opts);
        let mut db_opts = rocksdb::Options::default();
        db_opts.create_if_missing(true);
        db_opts.set_compaction_style(rocksdb::DBCompactionStyle::Level);
        db_opts.set_compression_type(rocksdb::DBCompressionType::Snappy);
        db_opts.set_target_file_size_base(128 << 20);
        db_opts.set_write_buffer_size(64 << 20);
        db_opts.increase_parallelism(2);
        db_opts.set_min_write_buffer_number(2);
        db_opts.set_max_write_buffer_number(3);
        db_opts.set_disable_auto_compactions(opts.bulk_import);
        db_opts.set_advise_random_on_open(!opts.bulk_import);

        let mut block_opts = rocksdb::BlockBasedOptions::default();
        block_opts.set_block_size(256 << 10);
        DBStore {
            db: rocksdb::DB::open(&db_opts, &path).unwrap(),
            path: path.to_owned(),
            opts: opts,
        }
    }

    pub fn bulk_load(self, rows: Receiver<Result<Vec<Vec<Row>>>>, signal: &Waiter) -> Result<()> {
        let key = b"F"; // full compaction marker
        if self.get(key).is_some() {
            return Ok(());
        }
        for rows in rows.iter() {
            if let Some(sig) = signal.poll() {
                bail!("indexing interrupted by SIG{:?}", sig);
            }
            self.write(rows?);
        }
        info!("starting full compaction");
        self.db.compact_range(None, None); // should take a while
        self.db.put(key, b"").unwrap();
        info!("finished full compaction");
        Ok(())
    }
}

impl ReadStore for DBStore {
    fn get(&self, key: &[u8]) -> Option<Bytes> {
        self.db.get(key).unwrap().map(|v| v.to_vec())
    }

    // TODO: use generators
    fn scan(&self, prefix: &[u8]) -> Vec<Row> {
        let mut rows = vec![];
        let mut iter = self.db.raw_iterator();
        iter.seek(prefix);
        while iter.valid() {
            let key = &iter.key().unwrap();
            if !key.starts_with(prefix) {
                break;
            }
            rows.push(Row {
                key: key.to_vec(),
                value: iter.value().unwrap().to_vec(),
            });
            iter.next();
        }
        rows
    }
}

impl WriteStore for DBStore {
    fn write(&self, rows_vec: Vec<Vec<Row>>) {
        let mut batch = rocksdb::WriteBatch::default();
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
        debug!("closing {}", self.path);
    }
}
