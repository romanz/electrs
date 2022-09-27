use std::fs::{create_dir_all, remove_dir_all};
use std::iter;
use std::path::Path;

use anyhow::{Context, Result};

use crate::db_store::{
    Config, DBStore, Row, WriteBatch, CONFIG_GROUP, CONFIG_KEY, CURRENT_FORMAT, FUNDING_GROUP,
    HEADERS_GROUP, SPENDING_GROUP, TIP_KEY, TXID_GROUP,
};

const CONFIG_TABLE: redb::TableDefinition<str, [u8]> = redb::TableDefinition::new(CONFIG_GROUP);
const FUNDING_TABLE: redb::TableDefinition<[u8], [u8; 0]> =
    redb::TableDefinition::new(FUNDING_GROUP);
const HEADERS_TABLE: redb::TableDefinition<[u8], [u8]> = redb::TableDefinition::new(HEADERS_GROUP);
const SPENDING_TABLE: redb::TableDefinition<[u8], [u8; 0]> =
    redb::TableDefinition::new(SPENDING_GROUP);
const TXID_TABLE: redb::TableDefinition<[u8], [u8; 0]> = redb::TableDefinition::new(TXID_GROUP);

/// redb wrapper for index storage
pub struct RedbStore {
    db: redb::Database,
}

fn builder() -> redb::DatabaseBuilder {
    let mut builder = redb::DatabaseBuilder::new();
    builder.set_write_strategy(redb::WriteStrategy::Throughput);
    builder.set_dynamic_growth(false);
    builder
}

impl RedbStore {
    fn open_internal(path: &Path) -> Result<Self> {
        let db_builder = builder();

        create_dir_all(path)?;
        let db: redb::Database = unsafe {
            db_builder
                .create(path.join("database.redb"), 8 * 1024 * 1024 * 1024)
                .with_context(|| format!("failed to open DB: {}", path.display()))?
        };
        let write_txn = db.begin_write()?;
        let db_stats = write_txn.stats()?;
        info!(
            "{:?}: Tree Height {}, {} Free Pages, {} Leaf Pages, {} Branch Pages, {} Stored Bytes, {} Metadata Bytes, {} Fragmented Bytes, {} Page Size",
            path,
            db_stats.tree_height(),
            db_stats.free_pages(),
            db_stats.leaf_pages(),
            db_stats.branch_pages(),
            db_stats.stored_bytes(),
            db_stats.metadata_bytes(),
            db_stats.fragmented_bytes(),
            db_stats.page_size(),
        );
        write_txn.abort()?;

        let write_txn = db.begin_write().expect("DB::begin_write failed");
        {
            write_txn
                .open_table(CONFIG_TABLE)
                .expect("WriteTransaction::open_table failed");
            write_txn
                .open_table(FUNDING_TABLE)
                .expect("WriteTransaction::open_table failed");
            write_txn
                .open_table(SPENDING_TABLE)
                .expect("WriteTransaction::open_table failed");
            write_txn
                .open_table(TXID_TABLE)
                .expect("WriteTransaction::open_table failed");
            write_txn
                .open_table(HEADERS_TABLE)
                .expect("WriteTransaction::open_table failed");
        }
        write_txn.commit().expect("WriteTransaction::commit failed");

        let store = RedbStore { db };
        Ok(store)
    }

    fn set_config(&self, config: Config) {
        let value = serde_json::to_vec(&config).expect("failed to serialize config");
        let write_txn = self.db.begin_write().expect("DB::begin_write failed");
        {
            let mut table = write_txn
                .open_table(CONFIG_TABLE)
                .expect("Table::open failed");
            table
                .insert(CONFIG_KEY, &value)
                .expect("Table::insert failed");
        }
        write_txn.commit().expect("WriteTransaction::commit failed");
    }

    fn get_config(&self) -> Option<Config> {
        let read_txn = self.db.begin_read().expect("DB::begin_read failed");
        let table = read_txn
            .open_table(CONFIG_TABLE)
            .expect("Table::open failed");
        redb::ReadableTable::get(&table, CONFIG_KEY)
            .expect("Table::get failed")
            .map(|value| serde_json::from_slice(&value).expect("failed to deserialize Config"))
    }
}

impl DBStore for RedbStore {
    /// Opens a new redb at the specified location.
    fn open(path: &Path, auto_reindex: bool) -> Result<Self> {
        let mut store = Self::open_internal(path)?;
        let config = store.get_config();
        debug!("DB {:?}", config);
        let mut config = config.unwrap_or_default(); // use default config when DB is empty

        let reindex_cause = if config.format != CURRENT_FORMAT {
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
            remove_dir_all(path).unwrap();
            store = Self::open_internal(path)?;
            config = Config::default(); // re-init config after dropping DB
        }
        store.set_config(config);
        Ok(store)
    }

    fn iter_funding(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_> {
        let read_txn = self.db.begin_read().expect("DB::begin_read failed");
        let table = read_txn
            .open_table(FUNDING_TABLE)
            .expect("DB::open_table failed");
        let mut iter = redb::ReadableTable::range(&table, prefix..).expect("Table::range failed");
        let mut vec: Vec<Row> = Vec::new();
        while let Some(pair) = iter.next() {
            let key = pair.0;
            vec.push(Row::from(key.to_vec()));
        }
        Box::new(vec.into_iter())
    }

    fn iter_spending(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_> {
        let read_txn = self.db.begin_read().expect("DB::begin_read failed");
        let table = read_txn
            .open_table(SPENDING_TABLE)
            .expect("DB::open_table failed");
        let mut iter = redb::ReadableTable::range(&table, prefix..).expect("Table::range failed");
        let mut vec: Vec<Row> = Vec::new();
        while let Some(pair) = iter.next() {
            let key = pair.0;
            vec.push(Row::from(key.to_vec()));
        }
        Box::new(vec.into_iter())
    }

    fn iter_txid(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_> {
        let read_txn = self.db.begin_read().expect("DB::begin_read failed");
        let table = read_txn
            .open_table(TXID_TABLE)
            .expect("DB::open_table failed");
        let mut iter = redb::ReadableTable::range(&table, prefix..).expect("Table::range failed");
        let mut vec: Vec<Row> = Vec::new();
        while let Some(pair) = iter.next() {
            let key = pair.0;
            vec.push(Row::from(key.to_vec()));
        }
        Box::new(vec.into_iter())
    }

    fn read_headers(&self) -> Vec<Row> {
        let read_txn = self.db.begin_read().expect("DB::begin_read failed");
        let table = read_txn
            .open_table(HEADERS_TABLE)
            .expect("DB::open_table failed");
        let mut iter = redb::ReadableTable::range(&table, []..).expect("Table::range failed");
        let mut vec: Vec<Row> = Vec::new();
        while let Some(pair) = iter.next() {
            let key = pair.0;
            if &key[..] != TIP_KEY {
                vec.push(Row::from(key.to_vec()));
            }
        }
        vec
    }

    fn get_tip(&self) -> Option<Vec<u8>> {
        let read_txn = self.db.begin_read().expect("DB::begin_read failed");
        let table = read_txn
            .open_table(HEADERS_TABLE)
            .expect("Table::open failed");
        redb::ReadableTable::get(&table, TIP_KEY)
            .expect("get_tip failed")
            .map(|value| value.to_vec())
    }

    fn write(&self, batch: &WriteBatch) {
        let write_txn = self.db.begin_write().expect("DB::begin_write failed");
        {
            let mut table = write_txn
                .open_table(FUNDING_TABLE)
                .expect("WriteTransaction::open_table failed");
            for key in &batch.funding_rows {
                table.insert(key, b"").expect("Table::insert failed");
            }
            let mut table = write_txn
                .open_table(SPENDING_TABLE)
                .expect("WriteTransaction::open_table failed");
            for key in &batch.spending_rows {
                table.insert(key, b"").expect("Table::insert failed");
            }
            let mut table = write_txn
                .open_table(TXID_TABLE)
                .expect("WriteTransaction::open_table failed");
            for key in &batch.txid_rows {
                table.insert(key, b"").expect("Table::insert failed");
            }
            let mut table = write_txn
                .open_table(HEADERS_TABLE)
                .expect("WriteTransaction::open_table failed");
            for key in &batch.header_rows {
                table.insert(key, b"").expect("Table::insert failed");
            }
            table
                .insert(TIP_KEY, &batch.tip_row)
                .expect("Table::insert failed");
        }

        write_txn.commit().expect("WriteTransaction::commit failed");
    }

    fn flush(&self) {}

    fn get_properties(&self) -> Box<dyn Iterator<Item = (&'static str, &'static str, u64)> + '_> {
        Box::new(iter::empty::<(&'static str, &'static str, u64)>())
    }
}

impl Drop for RedbStore {
    fn drop(&mut self) {
        info!("closing DB");
    }
}

#[cfg(test)]
mod tests {
    use crate::db_store::DBStore;

    use super::{RedbStore, WriteBatch, CURRENT_FORMAT};

    #[test]
    fn test_reindex_new_format() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = RedbStore::open(dir.path(), false).unwrap();
            let mut config = store.get_config().unwrap();
            config.format += 1;
            store.set_config(config);
        };
        assert_eq!(
            RedbStore::open(dir.path(), false)
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
            let store = RedbStore::open(dir.path(), true).unwrap();
            store.flush();
            let config = store.get_config().unwrap();
            assert_eq!(config.format, CURRENT_FORMAT);
        }
    }

    #[test]
    fn test_db_prefix_scan() {
        let dir = tempfile::tempdir().unwrap();
        let store = RedbStore::open(dir.path(), true).unwrap();

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
