use anyhow::{Context, Result};
use bitcoin::consensus::deserialize;
use cdb64::{Cdb, CdbHash, CdbWriter};
use rust_rocksdb as rocksdb;

use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::types::{
    HashPrefix, HashPrefixRow, SerializedHashPrefixRow, SerializedHeaderRow, HASH_PREFIX_LEN,
    HASH_PREFIX_ROW_SIZE, HEIGHT_SIZE,
};

#[derive(Default)]
pub(crate) struct WriteBatch {
    pub(crate) tip_row: [u8; 32],
    pub(crate) header_rows: Vec<SerializedHeaderRow>,
    pub(crate) funding_rows: Vec<SerializedHashPrefixRow>,
    pub(crate) spending_rows: Vec<SerializedHashPrefixRow>,
    pub(crate) txid_rows: Vec<SerializedHashPrefixRow>,
}

impl WriteBatch {
    pub(crate) fn sort(&mut self) {
        self.header_rows.sort_unstable();
        self.funding_rows.sort_unstable();
        self.spending_rows.sort_unstable();
        self.txid_rows.sort_unstable();
    }
}

/// RocksDB wrapper for index storage
pub struct DBStore {
    db: rocksdb::DB,
    bulk_import: AtomicBool,
    cdb_txid: Option<Cdb<File, CdbHash>>,
    cdb_funding: Option<Cdb<File, CdbHash>>,
    cdb_spending: Option<Cdb<File, CdbHash>>,
    cdb_finalized_block_height: Option<usize>,
    cached_cdb_cleanup_block_height: Option<usize>,
}

const CONFIG_CF: &str = "config";
const HEADERS_CF: &str = "headers";
const TXID_CF: &str = "txid";
const FUNDING_CF: &str = "funding";
const SPENDING_CF: &str = "spending";

const COLUMN_FAMILIES: &[&str] = &[CONFIG_CF, HEADERS_CF, TXID_CF, FUNDING_CF, SPENDING_CF];

const CONFIG_KEY: &str = "C";
const TIP_KEY: &[u8] = b"T";

// Taken from https://github.com/facebook/rocksdb/blob/master/include/rocksdb/db.h#L654-L689
const DB_PROPERTIES: &[&str] = &[
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

#[derive(Debug, Deserialize, Serialize)]
struct Config {
    compacted: bool,
    format: u64,
    cdb_cleanup_block_height: Option<usize>,
}

const CURRENT_FORMAT: u64 = 0;

impl Default for Config {
    fn default() -> Self {
        Config {
            compacted: false,
            format: CURRENT_FORMAT,
            cdb_cleanup_block_height: None,
        }
    }
}

fn default_opts(parallelism: u8) -> rocksdb::Options {
    let mut block_opts = rocksdb::BlockBasedOptions::default();
    block_opts.set_checksum_type(rocksdb::ChecksumType::CRC32c);

    let mut opts = rocksdb::Options::default();
    opts.increase_parallelism(parallelism.into());
    opts.set_max_subcompactions(parallelism.into());

    opts.set_keep_log_file_num(10);
    opts.set_max_open_files(16);
    opts.set_compaction_style(rocksdb::DBCompactionStyle::Level);
    opts.set_compression_type(rocksdb::DBCompressionType::Zstd);
    opts.set_target_file_size_base(256 << 20);
    opts.set_write_buffer_size(256 << 20);
    opts.set_disable_auto_compactions(true); // for initial bulk load
    opts.set_advise_random_on_open(false); // bulk load uses sequential I/O
    opts.set_prefix_extractor(rocksdb::SliceTransform::create_fixed_prefix(8));
    opts.set_block_based_table_factory(&block_opts);
    opts
}

/// Collect all heights for `prefix` from `raw` (advancing past them), keeping only
/// those <= `max_height`. Returns the heights as concatenated little-endian u32 bytes.
fn collect_rdb_prefix(
    rdb_iter: &mut rocksdb::DBRawIterator<'_>,
    prefix: HashPrefix,
    max_height: usize,
) -> Vec<u8> {
    let mut heights_buf = Vec::new();
    loop {
        let (key_prefix, height, height_bytes) = match rdb_iter.key() {
            Some(k) => {
                debug_assert_eq!(k.len(), HASH_PREFIX_ROW_SIZE);
                let kp: HashPrefix = k[..HASH_PREFIX_LEN].try_into().unwrap();
                let hb: [u8; HEIGHT_SIZE] = k[HASH_PREFIX_LEN..].try_into().unwrap();
                let h = deserialize::<u32>(&hb).expect("invalid height") as usize;
                (kp, h, hb)
            }
            None => {
                rdb_iter.status().expect("DB scan failed");
                break;
            }
        };
        if key_prefix != prefix {
            break;
        }
        if height <= max_height {
            heights_buf.extend_from_slice(&height_bytes);
        }
        rdb_iter.next();
    }
    heights_buf
}

impl DBStore {
    fn create_cf_descriptors(parallelism: u8) -> Vec<rocksdb::ColumnFamilyDescriptor> {
        COLUMN_FAMILIES
            .iter()
            .map(|&name| rocksdb::ColumnFamilyDescriptor::new(name, default_opts(parallelism)))
            .collect()
    }

    fn open_internal(path: &Path, log_dir: Option<&Path>, parallelism: u8) -> Result<Self> {
        let mut db_opts = default_opts(parallelism);
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);
        if let Some(d) = log_dir {
            db_opts.set_db_log_dir(d);
        }

        let db = rocksdb::DB::open_cf_descriptors(
            &db_opts,
            path,
            Self::create_cf_descriptors(parallelism),
        )
        .with_context(|| format!("failed to open DB: {}", path.display()))?;
        let live_files = db.live_files()?;
        info!(
            "{:?}: {} SST files, {} GB, {} Grows",
            path,
            live_files.len(),
            live_files.iter().map(|f| f.size).sum::<usize>() as f64 / 1e9,
            live_files.iter().map(|f| f.num_entries).sum::<u64>() as f64 / 1e9
        );
        let store = DBStore {
            db,
            bulk_import: AtomicBool::new(true),
            cdb_txid: None,
            cdb_funding: None,
            cdb_spending: None,
            cdb_finalized_block_height: None,
            cached_cdb_cleanup_block_height: None,
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

    /// Opens a new RocksDB at the specified location.
    pub fn open(
        path: &Path,
        log_dir: Option<&Path>,
        auto_reindex: bool,
        parallelism: u8,
    ) -> Result<Self> {
        let mut store = Self::open_internal(path, log_dir, parallelism)?;
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
            rocksdb::DB::destroy(&default_opts(parallelism), path).with_context(|| {
                format!(
                    "re-index required but the old database ({}) can not be deleted",
                    path.display()
                )
            })?;
            store = Self::open_internal(path, log_dir, parallelism)?;
            config = Config::default(); // re-init config after dropping DB
        }
        if config.compacted {
            store.start_compactions();
        }
        store.cached_cdb_cleanup_block_height = config.cdb_cleanup_block_height;
        store.set_config(config);
        Ok(store)
    }

    pub fn open_finalized_cdb(
        &mut self,
        cdb_path: &Path,
        finalized_block_height: usize,
    ) -> Result<()> {
        self.cdb_txid = Some(
            Cdb::<File, CdbHash>::open(
                cdb_path.join(format!("txid{}.cdb", finalized_block_height)),
            )
            .with_context(|| format!("failed to open txid CDB at {:?}", cdb_path))?,
        );
        self.cdb_funding = Some(
            Cdb::<File, CdbHash>::open(
                cdb_path.join(format!("funding{}.cdb", finalized_block_height)),
            )
            .with_context(|| format!("failed to open funding CDB at {:?}", cdb_path))?,
        );
        self.cdb_spending = Some(
            Cdb::<File, CdbHash>::open(
                cdb_path.join(format!("spending{}.cdb", finalized_block_height)),
            )
            .with_context(|| format!("failed to open spending CDB at {:?}", cdb_path))?,
        );
        self.cdb_finalized_block_height = Some(finalized_block_height);
        Ok(())
    }

    fn config_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(CONFIG_CF).expect("missing CONFIG_CF")
    }

    fn funding_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(FUNDING_CF).expect("missing FUNDING_CF")
    }

    fn spending_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(SPENDING_CF).expect("missing SPENDING_CF")
    }

    fn txid_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(TXID_CF).expect("missing TXID_CF")
    }

    fn headers_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(HEADERS_CF).expect("missing HEADERS_CF")
    }

    pub(crate) fn iter_funding_block_heights(
        &self,
        prefix: HashPrefix,
    ) -> Box<dyn Iterator<Item = usize> + '_> {
        if let (Some(cdb), Some(fh)) = (&self.cdb_funding, self.cdb_finalized_block_height) {
            Box::new(
                Self::iter_prefix_cdb(cdb, prefix).chain(
                    self.iter_prefix_cf(self.funding_cf(), prefix)
                        .filter(move |&h| h > fh),
                ),
            )
        } else {
            Box::new(self.iter_prefix_cf(self.funding_cf(), prefix))
        }
    }

    pub(crate) fn iter_spending_block_heights(
        &self,
        prefix: HashPrefix,
    ) -> Box<dyn Iterator<Item = usize> + '_> {
        if let (Some(cdb), Some(fh)) = (&self.cdb_spending, self.cdb_finalized_block_height) {
            Box::new(
                Self::iter_prefix_cdb(cdb, prefix).chain(
                    self.iter_prefix_cf(self.spending_cf(), prefix)
                        .filter(move |&h| h > fh),
                ),
            )
        } else {
            Box::new(self.iter_prefix_cf(self.spending_cf(), prefix))
        }
    }

    pub(crate) fn iter_txid_block_heights(
        &self,
        prefix: HashPrefix,
    ) -> Box<dyn Iterator<Item = usize> + '_> {
        if let (Some(cdb), Some(fh)) = (&self.cdb_txid, self.cdb_finalized_block_height) {
            Box::new(
                Self::iter_prefix_cdb(cdb, prefix).chain(
                    self.iter_prefix_cf(self.txid_cf(), prefix)
                        .filter(move |&h| h > fh),
                ),
            )
        } else {
            Box::new(self.iter_prefix_cf(self.txid_cf(), prefix))
        }
    }

    fn iter_cf<const N: usize>(
        &self,
        cf: &rocksdb::ColumnFamily,
        readopts: rocksdb::ReadOptions,
        prefix: Option<HashPrefix>,
    ) -> impl Iterator<Item = [u8; N]> + '_ {
        DBIterator::new(self.db.raw_iterator_cf_opt(cf, readopts), prefix)
    }

    pub(crate) fn iter_prefix_cdb(
        cdb: &Cdb<File, CdbHash>,
        prefix: HashPrefix,
    ) -> std::vec::IntoIter<usize> {
        let value = cdb
            .get(&prefix)
            .expect("CDB read failed")
            .unwrap_or_default();
        value
            .chunks_exact(HEIGHT_SIZE)
            .map(|height_chunk| deserialize::<u32>(height_chunk).expect("invalid height") as usize)
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn iter_prefix_cf(
        &self,
        cf: &rocksdb::ColumnFamily,
        prefix: HashPrefix,
    ) -> impl Iterator<Item = usize> + '_ {
        let mut opts = rocksdb::ReadOptions::default();
        opts.set_prefix_same_as_start(true); // requires .set_prefix_extractor() above.
        self.iter_cf::<{ crate::types::HASH_PREFIX_ROW_SIZE }>(cf, opts, Some(prefix))
            .map(|row| HashPrefixRow::from_db_row(row).height())
    }

    pub(crate) fn iter_headers(&self) -> impl Iterator<Item = SerializedHeaderRow> + '_ {
        let mut opts = rocksdb::ReadOptions::default();
        opts.fill_cache(false);
        self.iter_cf(self.headers_cf(), opts, None)
    }

    pub(crate) fn get_tip(&self) -> Option<Vec<u8>> {
        self.db
            .get_cf(self.headers_cf(), TIP_KEY)
            .expect("get_tip failed")
    }

    pub(crate) fn write(&self, batch: &WriteBatch) {
        let mut db_batch = rocksdb::WriteBatch::default();
        let funding_cf = self.funding_cf();
        for key in &batch.funding_rows {
            db_batch.put_cf(funding_cf, key, b"");
        }
        let spending_cf = self.spending_cf();
        for key in &batch.spending_rows {
            db_batch.put_cf(spending_cf, key, b"");
        }
        let txid_cf = self.txid_cf();
        for key in &batch.txid_rows {
            db_batch.put_cf(txid_cf, key, b"");
        }
        let headers_cf = self.headers_cf();
        for key in &batch.header_rows {
            db_batch.put_cf(headers_cf, key, b"");
        }
        db_batch.put_cf(headers_cf, TIP_KEY, batch.tip_row);

        let mut opts = rocksdb::WriteOptions::new();
        let bulk_import = self.bulk_import.load(Ordering::Relaxed);
        opts.set_sync(!bulk_import);
        opts.disable_wal(bulk_import);
        self.db.write_opt(db_batch, &opts).unwrap();
    }

    /// Scan all rows in `cf`, group by 8-byte prefix, keep only heights <= `max_height`,
    /// and write one CDB entry per prefix (value = concatenated little-endian u32 heights).
    fn build_cdb_for_cf(
        db: &rocksdb::DB,
        cf: &rocksdb::ColumnFamily,
        path: &Path,
        max_height: usize,
        existing_cdb: Option<&Cdb<File, CdbHash>>,
    ) -> Result<()> {
        let mut writer = CdbWriter::<File, CdbHash>::create(path)
            .with_context(|| format!("failed to create CDB writer at {:?}", path))?;

        let mut opts = rocksdb::ReadOptions::default();
        opts.fill_cache(false);
        let mut rdb_iter = db.raw_iterator_cf_opt(cf, opts);
        rdb_iter.seek_to_first();

        // Build a peekable iterator over (prefix, value) pairs from the existing CDB.
        // CDB's records section is simply the records in the order they were added, with no
        // reordering on finalization. Since we always build CDB by scanning RocksDB in sorted
        // key order, the CDB iterator yields prefixes in sorted order as well.
        let mut cdb_iter = existing_cdb.map(|c| {
            c.iter()
                .map(|r| {
                    let (k, v) = r.expect("CDB read failed");
                    let prefix: HashPrefix =
                        k[..HASH_PREFIX_LEN].try_into().expect("invalid CDB key");
                    (prefix, v)
                })
                .peekable()
        });

        // Merge-sort the two sorted streams.
        loop {
            let rdb_prefix: Option<HashPrefix> = rdb_iter.key().map(|k| {
                debug_assert_eq!(k.len(), HASH_PREFIX_ROW_SIZE);
                k[..HASH_PREFIX_LEN].try_into().unwrap()
            });
            let cdb_prefix: Option<HashPrefix> =
                cdb_iter.as_mut().and_then(|it| it.peek().map(|(p, _)| *p));

            match (rdb_prefix, cdb_prefix) {
                (None, None) => break,
                (None, Some(_)) => {
                    // Only CDB entries remain: copy them directly.
                    let (prefix, cdb_val) = cdb_iter.as_mut().unwrap().next().unwrap();
                    writer.put(&prefix, &cdb_val).context("CDB put failed")?;
                }
                (Some(rp), None) => {
                    // Only RocksDB entries remain: collect and write.
                    let heights_buf = collect_rdb_prefix(&mut rdb_iter, rp, max_height);
                    if !heights_buf.is_empty() {
                        writer.put(&rp, &heights_buf).context("CDB put failed")?;
                    }
                }
                (Some(rp), Some(cp)) => match rp.cmp(&cp) {
                    std::cmp::Ordering::Less => {
                        let heights_buf = collect_rdb_prefix(&mut rdb_iter, rp, max_height);
                        if !heights_buf.is_empty() {
                            writer.put(&rp, &heights_buf).context("CDB put failed")?;
                        }
                    }
                    std::cmp::Ordering::Greater => {
                        let (prefix, cdb_val) = cdb_iter.as_mut().unwrap().next().unwrap();
                        writer.put(&prefix, &cdb_val).context("CDB put failed")?;
                    }
                    std::cmp::Ordering::Equal => {
                        // Prefix in both: start with the CDB value, then append only RDB
                        // heights not already present in the CDB (deduplication needed because
                        // RocksDB still holds all heights until cleanup is implemented).
                        let (_, cdb_val) = cdb_iter.as_mut().unwrap().next().unwrap();
                        let cdb_heights: std::collections::HashSet<[u8; HEIGHT_SIZE]> = cdb_val
                            .chunks_exact(HEIGHT_SIZE)
                            .map(|c| c.try_into().unwrap())
                            .collect();
                        let mut heights_buf = cdb_val;
                        for chunk in collect_rdb_prefix(&mut rdb_iter, rp, max_height)
                            .chunks_exact(HEIGHT_SIZE)
                        {
                            if !cdb_heights.contains(chunk) {
                                heights_buf.extend_from_slice(chunk);
                            }
                        }
                        if !heights_buf.is_empty() {
                            writer.put(&rp, &heights_buf).context("CDB put failed")?;
                        }
                    }
                },
            }
        }

        writer
            .finalize()
            .with_context(|| format!("failed to finalize CDB at {:?}", path))?;
        Ok(())
    }

    /// Build CDB files for all three index column families (txid, funding, spending),
    /// covering blocks up to and including `max_height`. Renames tmp files to cdb and
    /// opens the resulting files as CDB readers.
    pub(crate) fn synchronize_cdb(&mut self, cdb_path: &Path, max_height: usize) -> Result<()> {
        let old_height = self.cdb_finalized_block_height;
        if let Some(current) = old_height {
            if current > max_height {
                bail!("cdb max-block-height cannot be decreased");
            }
            if current == max_height {
                return Ok(());
            }
        }

        let txid_tmp_path = cdb_path.join(format!("txid{}.cdb.tmp", max_height));
        let funding_tmp_path = cdb_path.join(format!("funding{}.cdb.tmp", max_height));
        let spending_tmp_path = cdb_path.join(format!("spending{}.cdb.tmp", max_height));

        info!(
            "building CDB for heights up to {} at {:?}",
            max_height, cdb_path
        );
        Self::build_cdb_for_cf(
            &self.db,
            self.txid_cf(),
            &txid_tmp_path,
            max_height,
            self.cdb_txid.as_ref(),
        )
        .context("failed to build txid CDB")?;
        Self::build_cdb_for_cf(
            &self.db,
            self.funding_cf(),
            &funding_tmp_path,
            max_height,
            self.cdb_funding.as_ref(),
        )
        .context("failed to build funding CDB")?;
        Self::build_cdb_for_cf(
            &self.db,
            self.spending_cf(),
            &spending_tmp_path,
            max_height,
            self.cdb_spending.as_ref(),
        )
        .context("failed to build spending CDB")?;

        let txid_path = cdb_path.join(format!("txid{}.cdb", max_height));
        let funding_path = cdb_path.join(format!("funding{}.cdb", max_height));
        let spending_path = cdb_path.join(format!("spending{}.cdb", max_height));
        std::fs::rename(&txid_tmp_path, &txid_path)
            .with_context(|| format!("failed to rename {:?} to {:?}", txid_tmp_path, txid_path))?;
        std::fs::rename(&funding_tmp_path, &funding_path).with_context(|| {
            format!(
                "failed to rename {:?} to {:?}",
                funding_tmp_path, funding_path
            )
        })?;
        std::fs::rename(&spending_tmp_path, &spending_path).with_context(|| {
            format!(
                "failed to rename {:?} to {:?}",
                spending_tmp_path, spending_path
            )
        })?;

        self.open_finalized_cdb(cdb_path, max_height)?;
        info!("CDB finalized at height {}", max_height);

        if let Some(old) = old_height {
            std::fs::remove_file(cdb_path.join(format!("txid{}.cdb", old)))
                .context("failed to delete old txid CDB")?;
            std::fs::remove_file(cdb_path.join(format!("funding{}.cdb", old)))
                .context("failed to delete old funding CDB")?;
            std::fs::remove_file(cdb_path.join(format!("spending{}.cdb", old)))
                .context("failed to delete old spending CDB")?;
            info!("deleted old CDB files for height {}", old);
        }

        Ok(())
    }

    fn build_cleanup_batch(cf: &rocksdb::ColumnFamily, db: &rocksdb::DB, max_height: usize) {
        const CHUNK_SIZE: usize = 100_000;
        let mut batch = rocksdb::WriteBatch::default();
        let mut batch_count = 0;
        let mut opts = rocksdb::ReadOptions::default();
        opts.fill_cache(false);
        let mut iter = db.raw_iterator_cf_opt(cf, opts);
        iter.seek_to_first();
        loop {
            match iter.key() {
                Some(k) => {
                    debug_assert_eq!(k.len(), HASH_PREFIX_ROW_SIZE);
                    let key: [u8; HASH_PREFIX_ROW_SIZE] = k.try_into().unwrap();
                    let height = deserialize::<u32>(&key[HASH_PREFIX_LEN..])
                        .expect("invalid height") as usize;
                    if height <= max_height {
                        batch.delete_cf(cf, key);
                        batch_count += 1;
                        if batch_count >= CHUNK_SIZE {
                            db.write(batch).expect("DB cleanup write failed");
                            batch = rocksdb::WriteBatch::default();
                            batch_count = 0;
                        }
                    }
                }
                None => {
                    iter.status().expect("DB scan failed");
                    break;
                }
            }
            iter.next();
        }
        if batch_count > 0 {
            db.write(batch).expect("DB cleanup write failed");
        }
    }

    /// Delete all RocksDB records in the index CFs with height <= `max_height`,
    /// since those heights are now covered by the CDB. Updates `cdb_cleanup_block_height`.
    pub(crate) fn cleanup_rdb_duplications(&mut self, max_height: usize) -> Result<()> {
        if self.cached_cdb_cleanup_block_height == Some(max_height) {
            return Ok(());
        }
        info!("cleaning up RocksDB records up to height {}", max_height);
        for cf in [self.txid_cf(), self.funding_cf(), self.spending_cf()] {
            Self::build_cleanup_batch(cf, &self.db, max_height);
        }
        let mut config = self.get_config().unwrap_or_default();
        config.cdb_cleanup_block_height = Some(max_height);
        self.set_config(config);
        self.cached_cdb_cleanup_block_height = Some(max_height);
        info!("RocksDB cleanup completed for height {}", max_height);
        Ok(())
    }

    pub(crate) fn flush(&self) {
        debug!("flushing DB column families");
        let mut config = self.get_config().unwrap_or_default();
        for name in COLUMN_FAMILIES {
            let cf = self.db.cf_handle(name).expect("missing CF");
            self.db.flush_cf(cf).expect("CF flush failed");
        }
        if !config.compacted {
            for name in COLUMN_FAMILIES {
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
            let stats = self
                .db
                .property_value("rocksdb.dbstats")
                .expect("failed to get property")
                .expect("missing property");
            trace!("RocksDB stats: {}", stats);
        }
    }

    pub(crate) fn get_properties(
        &self,
    ) -> impl Iterator<Item = (&'static str, &'static str, u64)> + '_ {
        COLUMN_FAMILIES.iter().flat_map(move |cf_name| {
            let cf = self.db.cf_handle(cf_name).expect("missing CF");
            DB_PROPERTIES.iter().filter_map(move |property_name| {
                let value = self
                    .db
                    .property_int_value_cf(cf, *property_name)
                    .expect("failed to get property");
                Some((*cf_name, *property_name, value?))
            })
        })
    }

    fn start_compactions(&self) {
        self.bulk_import.store(false, Ordering::Relaxed);
        for name in COLUMN_FAMILIES {
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

struct DBIterator<'a, const N: usize> {
    raw: rocksdb::DBRawIterator<'a>,
    prefix: Option<HashPrefix>,
    done: bool,
}

impl<'a, const N: usize> DBIterator<'a, N> {
    fn new(mut raw: rocksdb::DBRawIterator<'a>, prefix: Option<HashPrefix>) -> Self {
        match prefix {
            Some(key) => raw.seek(key),
            None => raw.seek_to_first(),
        };
        Self {
            raw,
            prefix,
            done: false,
        }
    }
}

impl<const N: usize> Iterator for DBIterator<'_, N> {
    type Item = [u8; N];

    fn next(&mut self) -> Option<Self::Item> {
        while !self.done {
            let key = match self.raw.key() {
                Some(key) => key,
                None => {
                    self.raw.status().expect("DB scan failed");
                    break; // end of scan
                }
            };
            let prefix_match = match self.prefix {
                Some(key_prefix) => key.starts_with(&key_prefix),
                None => true,
            };
            if !prefix_match {
                break; // prefix mismatch
            }
            let result: Option<[u8; N]> = key.try_into().ok();
            self.raw.next();
            match result {
                Some(value) => return Some(value),
                None => continue, // skip keys with size != N
            }
        }
        self.done = true;
        None
    }
}

impl Drop for DBStore {
    fn drop(&mut self) {
        info!("closing DB at {}", self.db.path().display());
    }
}

#[cfg(test)]
mod tests {
    use super::{rocksdb, DBStore, WriteBatch, CURRENT_FORMAT};
    use std::ffi::{OsStr, OsString};
    use std::path::Path;

    #[test]
    fn test_reindex_new_format() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = DBStore::open(dir.path(), None, false, 1).unwrap();
            let mut config = store.get_config().unwrap();
            config.format += 1;
            store.set_config(config);
        };
        assert_eq!(
            DBStore::open(dir.path(), None, false, 1)
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
            let store = DBStore::open(dir.path(), None, true, 1).unwrap();
            store.flush();
            let config = store.get_config().unwrap();
            assert_eq!(config.format, CURRENT_FORMAT);
            assert!(!store.is_legacy_format());
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
            DBStore::open(dir.path(), None, false, 1)
                .err()
                .unwrap()
                .to_string(),
            format!("re-index required due to legacy format",)
        );
        {
            let store = DBStore::open(dir.path(), None, true, 1).unwrap();
            store.flush();
            let config = store.get_config().unwrap();
            assert_eq!(config.format, CURRENT_FORMAT);
        }
    }

    #[test]
    fn test_db_prefix_scan() {
        let dir = tempfile::tempdir().unwrap();
        let store = DBStore::open(dir.path(), None, true, 1).unwrap();

        let items = [
            *b"ab          ",
            *b"abcdefgh    ",
            *b"abcdefghj   ",
            *b"abcdefghjk  ",
            *b"abcdefghxyz ",
            *b"abcdefgi    ",
            *b"b           ",
            *b"c           ",
        ];

        let block_heights = [
            u32::from_le_bytes(*b"    "),
            u32::from_le_bytes(*b"j   "),
            u32::from_le_bytes(*b"jk  "),
            u32::from_le_bytes(*b"xyz "),
        ]
        .map(|h| h as usize);

        store.write(&WriteBatch {
            txid_rows: items.to_vec(),
            ..Default::default()
        });

        let rows = store.iter_txid_block_heights(*b"abcdefgh");
        assert_eq!(rows.collect::<Vec<_>>(), block_heights);
    }

    #[test]
    fn test_db_log_in_same_dir() {
        let dir1 = tempfile::tempdir().unwrap();
        let _store = DBStore::open(dir1.path(), None, true, 1).unwrap();

        // LOG file is created in dir1
        let dir_files = list_log_files(dir1.path());
        assert_eq!(dir_files, vec![OsStr::new("LOG")]);

        let dir2 = tempfile::tempdir().unwrap();
        let dir3 = tempfile::tempdir().unwrap();
        let _store = DBStore::open(dir2.path(), Some(dir3.path()), true, 1).unwrap();

        // *_LOG file is not created in dir2, but in dir3
        let dir_files = list_log_files(dir2.path());
        assert_eq!(dir_files, Vec::<OsString>::new());

        let dir_files = list_log_files(dir3.path());
        assert_eq!(dir_files.len(), 1);
        assert!(dir_files[0].to_str().unwrap().ends_with("_LOG"));
    }

    fn list_log_files(path: &Path) -> Vec<OsString> {
        path.read_dir()
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|e| e.to_str().unwrap().contains("LOG"))
            .collect()
    }
}
