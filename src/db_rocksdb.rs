use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use electrs_rocksdb as rocksdb;

use crate::db_store::{
    Config, DBStore, Row, WriteBatch, CONFIG_GROUP, CONFIG_KEY, CURRENT_FORMAT, FUNDING_GROUP,
    GROUPS, HEADERS_GROUP, SPENDING_GROUP, TIP_KEY, TXID_GROUP,
};

/// RocksDB wrapper for index storage
pub struct RocksDBStore {
    db: rocksdb::DB,
    bulk_import: AtomicBool,
}

// Taken from https://github.com/facebook/rocksdb/blob/master/include/rocksdb/db.h#L654-L689
const DB_PROPERIES: &[&str] = &[
    "rocksdb.num-immutable-mem-table",
    "rocksdb.mem-table-flush-pending",
    "rocksdb.compaction-pending",
    "rocksdb.background-errors",
    "rocksdb.cur-size-active-mem-table",
    "rocksdb.cur-size-all-mem-tables",
    "rocksdb.size-all-mem-tables",
    "rocksdb.num-entries-active-mem-table",
    "rocksdb.num-entries-imm-mem-tables",
    "rocksdb.num-deletes-active-mem-table",
    "rocksdb.num-deletes-imm-mem-tables",
    "rocksdb.estimate-num-keys",
    "rocksdb.estimate-table-readers-mem",
    "rocksdb.is-file-deletions-enabled",
    "rocksdb.num-snapshots",
    "rocksdb.oldest-snapshot-time",
    "rocksdb.num-live-versions",
    "rocksdb.current-super-version-number",
    "rocksdb.estimate-live-data-size",
    "rocksdb.min-log-number-to-keep",
    "rocksdb.min-obsolete-sst-number-to-keep",
    "rocksdb.total-sst-files-size",
    "rocksdb.live-sst-files-size",
    "rocksdb.base-level",
    "rocksdb.estimate-pending-compaction-bytes",
    "rocksdb.num-running-compactions",
    "rocksdb.num-running-flushes",
    "rocksdb.actual-delayed-write-rate",
    "rocksdb.is-write-stopped",
    "rocksdb.estimate-oldest-key-time",
    "rocksdb.block-cache-capacity",
    "rocksdb.block-cache-usage",
    "rocksdb.block-cache-pinned-usage",
];

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
    opts
}

impl RocksDBStore {
    fn create_cf_descriptors() -> Vec<rocksdb::ColumnFamilyDescriptor> {
        GROUPS
            .iter()
            .map(|&name| rocksdb::ColumnFamilyDescriptor::new(name, default_opts()))
            .collect()
    }

    fn open_internal(path: &Path) -> Result<Self> {
        let mut db_opts = default_opts();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);

        let db = rocksdb::DB::open_cf_descriptors(&db_opts, path, Self::create_cf_descriptors())
            .with_context(|| format!("failed to open DB: {}", path.display()))?;
        let live_files = db.live_files()?;
        info!(
            "{:?}: {} SST files, {} GB, {} Grows",
            path,
            live_files.len(),
            live_files.iter().map(|f| f.size).sum::<usize>() as f64 / 1e9,
            live_files.iter().map(|f| f.num_entries).sum::<u64>() as f64 / 1e9
        );
        let store = RocksDBStore {
            db,
            bulk_import: AtomicBool::new(true),
        };
        Ok(store)
    }

    fn is_legacy_format(&self) -> bool {
        // In legacy DB format, all data was stored in a single (default) column family.
        self.db
            .iterator(rocksdb::IteratorMode::Start)
            .next()
            .is_some()
    }

    fn config_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(CONFIG_GROUP).expect("missing CONFIG_CF")
    }

    fn funding_cf(&self) -> &rocksdb::ColumnFamily {
        self.db
            .cf_handle(FUNDING_GROUP)
            .expect("missing FUNDING_CF")
    }

    fn spending_cf(&self) -> &rocksdb::ColumnFamily {
        self.db
            .cf_handle(SPENDING_GROUP)
            .expect("missing SPENDING_CF")
    }

    fn txid_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(TXID_GROUP).expect("missing TXID_CF")
    }

    fn headers_cf(&self) -> &rocksdb::ColumnFamily {
        self.db
            .cf_handle(HEADERS_GROUP)
            .expect("missing HEADERS_CF")
    }

    fn iter_prefix_cf(
        &self,
        cf: &rocksdb::ColumnFamily,
        prefix: Row,
    ) -> impl Iterator<Item = Row> + '_ {
        let mode = rocksdb::IteratorMode::From(&prefix, rocksdb::Direction::Forward);
        let mut opts = rocksdb::ReadOptions::default();
        opts.set_prefix_same_as_start(true); // requires .set_prefix_extractor() above.
        self.db
            .iterator_cf_opt(cf, opts, mode)
            .map(|(key, _value)| key) // values are empty in prefix-scanned CFs
    }

    fn start_compactions(&self) {
        self.bulk_import.store(false, Ordering::Relaxed);
        for name in GROUPS {
            let cf = self.db.cf_handle(name).expect("missing CF");
            self.db
                .set_options_cf(cf, &[("disable_auto_compactions", "false")])
                .expect("failed to start auto-compactions");
        }
        debug!("auto-compactions enabled");
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

    fn get_config(&self) -> Option<Config> {
        self.db
            .get_cf(self.config_cf(), CONFIG_KEY)
            .expect("DB::get failed")
            .map(|value| serde_json::from_slice(&value).expect("failed to deserialize Config"))
    }
}

impl DBStore for RocksDBStore {
    /// Opens a new RocksDB at the specified location.
    fn open(path: &Path, auto_reindex: bool) -> Result<Self> {
        let mut store = Self::open_internal(path)?;
        let config = store.get_config();
        debug!("DB {:?}", config);
        let mut config = config.unwrap_or_default(); // use default config when DB is empty

        let reindex_cause = if store.is_legacy_format() {
            Some("legacy format".to_owned())
        } else if config.format != CURRENT_FORMAT {
            Some(format!(
                "unsupported format {} != {}",
                config.format, CURRENT_FORMAT
            ))
        } else {
            None
        };
        if let Some(cause) = reindex_cause {
            if !auto_reindex {
                bail!("re-index required due to {}", cause);
            }
            warn!(
                "Database needs to be re-indexed due to {}, going to delete {}",
                cause,
                path.display()
            );
            // close DB before deletion
            drop(store);
            rocksdb::DB::destroy(&default_opts(), &path).with_context(|| {
                format!(
                    "re-index required but the old database ({}) can not be deleted",
                    path.display()
                )
            })?;
            store = Self::open_internal(path)?;
            config = Config::default(); // re-init config after dropping DB
        }
        if config.compacted {
            store.start_compactions();
        }
        store.set_config(config);
        Ok(store)
    }

    fn iter_funding(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_> {
        Box::new(self.iter_prefix_cf(self.funding_cf(), prefix))
    }

    fn iter_spending(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_> {
        Box::new(self.iter_prefix_cf(self.spending_cf(), prefix))
    }

    fn iter_txid(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_> {
        Box::new(self.iter_prefix_cf(self.txid_cf(), prefix))
    }

    fn read_headers(&self) -> Vec<Row> {
        let mut opts = rocksdb::ReadOptions::default();
        opts.fill_cache(false);
        self.db
            .iterator_cf_opt(self.headers_cf(), opts, rocksdb::IteratorMode::Start)
            .map(|(key, _)| key)
            .filter(|key| &key[..] != TIP_KEY) // headers' rows are longer than TIP_KEY
            .collect()
    }

    fn get_tip(&self) -> Option<Vec<u8>> {
        self.db
            .get_cf(self.headers_cf(), TIP_KEY)
            .expect("get_tip failed")
    }

    fn write(&self, batch: &WriteBatch) {
        let mut db_batch = rocksdb::WriteBatch::default();
        for key in &batch.funding_rows {
            db_batch.put_cf(self.funding_cf(), key, b"");
        }
        for key in &batch.spending_rows {
            db_batch.put_cf(self.spending_cf(), key, b"");
        }
        for key in &batch.txid_rows {
            db_batch.put_cf(self.txid_cf(), key, b"");
        }
        for key in &batch.header_rows {
            db_batch.put_cf(self.headers_cf(), key, b"");
        }
        db_batch.put_cf(self.headers_cf(), TIP_KEY, &batch.tip_row);

        let mut opts = rocksdb::WriteOptions::new();
        let bulk_import = self.bulk_import.load(Ordering::Relaxed);
        opts.set_sync(!bulk_import);
        opts.disable_wal(bulk_import);
        self.db.write_opt(db_batch, &opts).unwrap();
    }

    fn flush(&self) {
        let mut config = self.get_config().unwrap_or_default();
        for name in GROUPS {
            let cf = self.db.cf_handle(name).expect("missing CF");
            self.db.flush_cf(cf).expect("CF flush failed");
        }
        if !config.compacted {
            for name in GROUPS {
                info!("starting {} compaction", name);
                let cf = self.db.cf_handle(name).expect("missing CF");
                self.db.compact_range_cf(cf, None::<&[u8]>, None::<&[u8]>);
            }
            config.compacted = true;
            self.set_config(config);
            info!("finished full compaction");
            self.start_compactions();
        }
        if log_enabled!(log::Level::Trace) {
            for property in &["rocksdb.dbstats"] {
                let stats = self
                    .db
                    .property_value(property)
                    .expect("failed to get property")
                    .expect("missing property");
                trace!("{}: {}", property, stats);
            }
        }
    }

    fn get_properties(&self) -> Box<dyn Iterator<Item = (&'static str, &'static str, u64)> + '_> {
        Box::new(GROUPS.iter().flat_map(move |cf_name| {
            let cf = self.db.cf_handle(cf_name).expect("missing CF");
            DB_PROPERIES.iter().filter_map(move |property_name| {
                let value = self
                    .db
                    .property_int_value_cf(cf, property_name)
                    .expect("failed to get property");
                Some((*cf_name, *property_name, value?))
            })
        }))
    }
}

impl Drop for RocksDBStore {
    fn drop(&mut self) {
        info!("closing DB at {}", self.db.path().display());
    }
}

#[cfg(test)]
mod tests {
    use crate::db_store::DBStore;

    use super::{rocksdb, RocksDBStore, WriteBatch, CURRENT_FORMAT};

    #[test]
    fn test_reindex_new_format() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = RocksDBStore::open(dir.path(), false).unwrap();
            let mut config = store.get_config().unwrap();
            config.format += 1;
            store.set_config(config);
        };
        assert_eq!(
            RocksDBStore::open(dir.path(), false)
                .err()
                .unwrap()
                .to_string(),
            format!(
                "re-index required due to unsupported format {} != {}",
                CURRENT_FORMAT + 1,
                CURRENT_FORMAT
            )
        );
        {
            let store = RocksDBStore::open(dir.path(), true).unwrap();
            store.flush();
            let config = store.get_config().unwrap();
            assert_eq!(config.format, CURRENT_FORMAT);
            assert_eq!(store.is_legacy_format(), false);
        }
    }

    #[test]
    fn test_reindex_legacy_format() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut db_opts = rocksdb::Options::default();
            db_opts.create_if_missing(true);
            let db = rocksdb::DB::open(&db_opts, dir.path()).unwrap();
            db.put(b"F", b"").unwrap(); // insert legacy DB compaction marker (in 'default' column family)
        };
        assert_eq!(
            RocksDBStore::open(dir.path(), false)
                .err()
                .unwrap()
                .to_string(),
            format!("re-index required due to legacy format",)
        );
        {
            let store = RocksDBStore::open(dir.path(), true).unwrap();
            store.flush();
            let config = store.get_config().unwrap();
            assert_eq!(config.format, CURRENT_FORMAT);
        }
    }

    #[test]
    fn test_db_prefix_scan() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksDBStore::open(dir.path(), true).unwrap();

        let items: &[&[u8]] = &[
            b"ab",
            b"abcdefgh",
            b"abcdefghj",
            b"abcdefghjk",
            b"abcdefghxyz",
            b"abcdefgi",
            b"b",
            b"c",
        ];

        let mut batch = WriteBatch::default();
        batch.txid_rows = to_rows(&items);
        store.write(&batch);

        let rows = store.iter_txid(b"abcdefgh".to_vec().into_boxed_slice());
        assert_eq!(rows.collect::<Vec<_>>(), to_rows(&items[1..5]));
    }

    fn to_rows(values: &[&[u8]]) -> Vec<Box<[u8]>> {
        values
            .iter()
            .map(|v| v.to_vec().into_boxed_slice())
            .collect()
    }
}
