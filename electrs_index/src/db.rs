use anyhow::{Context, Result};

use std::path::{Path, PathBuf};

pub(crate) type Row = Box<[u8]>;

pub(crate) struct WriteBatch {
    pub(crate) header_rows: Vec<Row>,
    pub(crate) index_rows: Vec<Row>,
}

#[derive(Debug)]
struct Options {
    path: PathBuf,
    bulk_import: bool,
}

pub struct DBStore {
    db: rocksdb::DB,
    opts: Options,
}

const CONFIG_CF: &str = "config";
const HEADERS_CF: &str = "headers";
const INDEX_CF: &str = "index";

const CONFIG_KEY: &str = "C";

#[derive(Debug, Deserialize, Serialize)]
struct Config {
    compacted: bool,
    format: u64,
}

const CURRENT_FORMAT: u64 = 1;

fn default_opts() -> rocksdb::Options {
    let mut opts = rocksdb::Options::default();
    opts.set_keep_log_file_num(10);
    opts.set_max_open_files(16);
    opts.set_compaction_style(rocksdb::DBCompactionStyle::Level);
    opts.set_compression_type(rocksdb::DBCompressionType::Zstd);
    opts.set_target_file_size_base(256 << 20);
    opts.set_write_buffer_size(256 << 20);
    opts.set_disable_auto_compactions(true); // for initial bulk load
    opts.set_advise_random_on_open(false); // bulk load uses sequential I/O
    opts.set_prefix_extractor(rocksdb::SliceTransform::create_fixed_prefix(8));
    opts.set_compaction_readahead_size(1 << 20);
    opts
}

impl DBStore {
    fn open_opts(opts: Options) -> Result<Self> {
        debug!("opening DB with {:?}", opts);
        let mut db_opts = default_opts();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);

        let cf_descriptors = vec![
            rocksdb::ColumnFamilyDescriptor::new(CONFIG_CF, default_opts()),
            rocksdb::ColumnFamilyDescriptor::new(HEADERS_CF, default_opts()),
            rocksdb::ColumnFamilyDescriptor::new(INDEX_CF, default_opts()),
        ];

        let db = rocksdb::DB::open_cf_descriptors(&db_opts, &opts.path, cf_descriptors)
            .context("failed to open DB")?;
        let mut store = DBStore { db, opts };

        let config = store.get_config();
        debug!("DB {:?}", config);
        if config.format < CURRENT_FORMAT {
            bail!(
                "unsupported storage format {}, re-index required",
                config.format
            );
        }
        if config.compacted {
            store.opts.bulk_import = false;
        }
        store.set_config(config);
        Ok(store)
    }

    fn config_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(CONFIG_CF).expect("missing CONFIG_CF")
    }

    fn index_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(INDEX_CF).expect("missing INDEX_CF")
    }

    fn headers_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(HEADERS_CF).expect("missing HEADERS_CF")
    }

    /// Opens a new RocksDB at the specified location.
    pub fn open(path: &Path) -> Result<Self> {
        DBStore::open_opts(Options {
            path: path.to_path_buf(),
            bulk_import: true,
        })
    }

    pub(crate) fn iter_index<'a>(&'a self, prefix: &'a [u8]) -> ScanIterator<'a> {
        let mode = rocksdb::IteratorMode::From(prefix, rocksdb::Direction::Forward);
        let iter = self.db.iterator_cf(self.index_cf(), mode);
        ScanIterator {
            prefix,
            iter,
            done: false,
        }
    }

    pub(crate) fn read_headers(&self) -> Vec<Row> {
        let mut opts = rocksdb::ReadOptions::default();
        opts.fill_cache(false);
        self.db
            .iterator_cf_opt(self.headers_cf(), opts, rocksdb::IteratorMode::Start)
            .map(|(key, _)| key)
            .collect()
    }

    pub(crate) fn write(&self, batch: WriteBatch) {
        let mut db_batch = rocksdb::WriteBatch::default();
        for key in batch.index_rows {
            db_batch.put_cf(self.index_cf(), key, b"");
        }
        for key in batch.header_rows {
            db_batch.put_cf(self.headers_cf(), key, b"");
        }
        let mut opts = rocksdb::WriteOptions::new();
        opts.set_sync(!self.opts.bulk_import);
        opts.disable_wal(self.opts.bulk_import);
        self.db.write_opt(db_batch, &opts).unwrap();
    }

    pub(crate) fn flush(&mut self) {
        let mut config = self.get_config();
        self.db.flush_cf(self.index_cf()).expect("index flush failed");
        self.db.flush_cf(self.headers_cf()).expect("headers flush failed");
        if !config.compacted {
            info!("starting full compaction");
            let cf = self.index_cf();
            self.db.compact_range_cf(cf, None::<&[u8]>, None::<&[u8]>); // would take a while
            info!("finished full compaction");

            config.compacted = true;
            self.set_config(config);
        }
        if self.opts.bulk_import {
            self.opts.bulk_import = false;
            self.db
                .set_options(&[("disable_auto_compactions", "false")])
                .unwrap();
            info!("auto-compactions enabled");
        }
    }

    fn set_config(&self, config: Config) {
        let mut opts = rocksdb::WriteOptions::default();
        opts.set_sync(true);
        opts.disable_wal(false);
        let value = serde_json::to_vec(&config).expect("failed to serialize config");
        self.db
            .put_cf_opt(self.config_cf(), CONFIG_KEY, value, &opts)
            .expect("DB::put failed");
    }

    fn get_config(&self) -> Config {
        self.db
            .get_cf(self.config_cf(), CONFIG_KEY)
            .expect("DB::get failed")
            .map(|value| serde_json::from_slice(&value).expect("failed to deserialize Config"))
            .unwrap_or_else(|| Config {
                compacted: false,
                format: CURRENT_FORMAT,
            })
    }
}

pub(crate) struct ScanIterator<'a> {
    prefix: &'a [u8],
    iter: rocksdb::DBIterator<'a>,
    done: bool,
}

impl<'a> Iterator for ScanIterator<'a> {
    type Item = Row;

    fn next(&mut self) -> Option<Row> {
        if self.done {
            return None;
        }
        let (key, _) = self.iter.next()?;
        if !key.starts_with(&self.prefix) {
            self.done = true;
            return None;
        }
        Some(key)
    }
}

impl Drop for DBStore {
    fn drop(&mut self) {
        info!("closing DB at {:?}", self.opts.path);
    }
}
