use std::fs::remove_dir_all;
use std::iter;
use std::path::Path;

use anyhow::{Context, Result};
use lmdb::{Cursor, DatabaseFlags, EnvironmentFlags, Transaction, WriteFlags};

use crate::db_store::{
    Config, DBStore, Row, WriteBatch, CONFIG_GROUP, CONFIG_KEY, CURRENT_FORMAT, FUNDING_GROUP,
    HEADERS_GROUP, SPENDING_GROUP, TIP_KEY, TXID_GROUP,
};

/// lmdb-rkv wrapper for index storage
pub struct LmdbStore {
    env: lmdb::Environment,
    config_db: lmdb::Database,
    funding_db: lmdb::Database,
    spending_db: lmdb::Database,
    txid_db: lmdb::Database,
    headers_db: lmdb::Database,
}

fn default_env_flags() -> lmdb::EnvironmentFlags {
    let mut env_flags = EnvironmentFlags::empty();

    env_flags.set(EnvironmentFlags::WRITE_MAP, true);
    env_flags.set(EnvironmentFlags::NO_META_SYNC, true);
    env_flags.set(EnvironmentFlags::NO_SYNC, true);
    env_flags.set(EnvironmentFlags::NO_READAHEAD, true);

    env_flags
}

fn default_db_flags() -> lmdb::DatabaseFlags {
    let db_flags = DatabaseFlags::empty();

    db_flags
}

fn default_write_flags() -> lmdb::WriteFlags {
    let write_flags = WriteFlags::empty();

    write_flags
}

impl LmdbStore {
    fn open_internal(path: &Path) -> Result<Self> {
        let mut env_builder = lmdb::Environment::new();
        env_builder.set_flags(default_env_flags());
        env_builder.set_max_dbs(6);
        env_builder.set_map_size(4 * 1024 * 1024);

        let env = env_builder
            .open(path)
            .with_context(|| format!("failed to open DB: {}", path.display()))?;
        let env_stat = env
            .stat()
            .with_context(|| format!("failed to open DB stats"))?;
        info!(
            "{:?}: Page Size {}, Depth {}, {} Branch Pages, {} Leaf Pages, {} Overflow Pages, {} Entries",
            path,
            env_stat.page_size(),
            env_stat.depth(),
            env_stat.branch_pages(),
            env_stat.leaf_pages(),
            env_stat.overflow_pages(),
            env_stat.entries(),
        );

        let config_db = env
            .create_db(Some(CONFIG_GROUP), default_db_flags())
            .expect("missing CONFIG_DB");
        let funding_db = env
            .create_db(Some(FUNDING_GROUP), default_db_flags())
            .expect("missing FUNDING_DB");
        let spending_db = env
            .create_db(Some(SPENDING_GROUP), default_db_flags())
            .expect("missing SPENDING_DB");
        let txid_db = env
            .create_db(Some(TXID_GROUP), default_db_flags())
            .expect("missing TXID_DB");
        let headers_db = env
            .create_db(Some(HEADERS_GROUP), default_db_flags())
            .expect("missing HEADERS_DB");

        let store = LmdbStore {
            env,
            config_db,
            funding_db,
            spending_db,
            txid_db,
            headers_db,
        };
        Ok(store)
    }

    fn iter_prefix_tree(&self, db: &lmdb::Database, prefix: Row) -> impl Iterator<Item = Row> + '_ {
        let read_txn = self.env.begin_ro_txn().expect("Env::begin_ro_txn failed");
        let mut read_cursor = read_txn
            .open_ro_cursor(*db)
            .expect("ReadTransaction::open_ro_cursor failed");
        let iter = read_cursor.iter_from(prefix);
        iter.map(|res| {
            if let Ok(res_ok) = res {
                Row::from(&res_ok.0[..])
            } else {
                Box::new([])
            }
        })
    }

    fn set_config(&self, config: Config) {
        let value = serde_json::to_vec(&config).expect("failed to serialize config");

        let mut write_txn = self.env.begin_rw_txn().expect("Env::begin_rw_txn failed");
        write_txn
            .put(self.config_db, &CONFIG_KEY, &value, default_write_flags())
            .expect("WriteTransaction::put failed");
        write_txn.commit().expect("WriteTransaction::Commit failed");
    }

    fn get_config(&self) -> Option<Config> {
        let read_txn = self.env.begin_ro_txn().expect("Env::begin_ro_txn failed");
        let value = read_txn.get(self.config_db, &CONFIG_KEY);

        if let Ok(value_ok) = value {
            serde_json::from_slice(&value_ok).expect("failed to deserialize Config")
        } else {
            None
        }
    }
}

impl DBStore for LmdbStore {
    /// Opens a new lmdb-rkv DB at the specified location.
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
            let mut write_txn = store.env.begin_rw_txn().expect("Env::begin_rw_txn failed");
            unsafe {
                write_txn
                    .drop_db(store.config_db)
                    .expect("WriteTransaction::drop_db failed");
                write_txn
                    .drop_db(store.funding_db)
                    .expect("WriteTransaction::drop_db failed");
                write_txn
                    .drop_db(store.spending_db)
                    .expect("WriteTransaction::drop_db failed");
                write_txn
                    .drop_db(store.txid_db)
                    .expect("WriteTransaction::drop_db failed");
                write_txn
                    .drop_db(store.headers_db)
                    .expect("WriteTransaction::drop_db failed");
            };
            write_txn.commit().expect("WriteTransaction::commit failed");
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
        Box::new(self.iter_prefix_tree(&self.funding_db, prefix))
    }

    fn iter_spending(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_> {
        Box::new(self.iter_prefix_tree(&self.spending_db, prefix))
    }

    fn iter_txid(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_> {
        Box::new(self.iter_prefix_tree(&self.txid_db, prefix))
    }

    fn read_headers(&self) -> Vec<Row> {
        let read_txn = self.env.begin_ro_txn().expect("Env::begin_ro_txn failed");
        let mut read_cursor = read_txn
            .open_ro_cursor(self.headers_db)
            .expect("ReadTransaction::open_ro_cursor failed");
        let iter = read_cursor.iter_start();
        iter.map(|res| Row::from(&res.unwrap().0[..]))
            .filter(|key| &key[..] != TIP_KEY) // headers' rows are longer than TIP_KEY
            .collect()
    }

    fn get_tip(&self) -> Option<Vec<u8>> {
        let read_txn = self.env.begin_ro_txn().expect("Env::begin_ro_txn failed");
        let tip = read_txn
            .get(self.headers_db, &TIP_KEY)
            .expect("ReadTransaction::get failed");

        Some(tip.to_vec())
    }

    fn write(&self, batch: &WriteBatch) {
        let mut write_txn = self.env.begin_rw_txn().expect("Env::begin_rw_txn failed");

        for key in &batch.funding_rows {
            write_txn
                .put(self.funding_db, &key, b"", default_write_flags())
                .expect("WriteTransaction::put failed");
        }

        for key in &batch.spending_rows {
            write_txn
                .put(self.spending_db, &key, b"", default_write_flags())
                .expect("WriteTransaction::put failed");
        }

        for key in &batch.txid_rows {
            write_txn
                .put(self.txid_db, &key, b"", default_write_flags())
                .expect("WriteTransaction::put failed");
        }

        for key in &batch.header_rows {
            write_txn
                .put(self.headers_db, &key, b"", default_write_flags())
                .expect("WriteTransaction::put failed");
        }
        write_txn
            .put(
                self.headers_db,
                &TIP_KEY,
                &batch.tip_row,
                default_write_flags(),
            )
            .expect("WriteTransaction::put failed");

        write_txn.commit().expect("WriteTransaction::commit failed");
    }

    fn flush(&self) {
        self.env.sync(false).expect("Env::Synv failed");
    }

    fn get_properties(&self) -> Box<dyn Iterator<Item = (&'static str, &'static str, u64)> + '_> {
        Box::new(iter::empty::<(&'static str, &'static str, u64)>())
    }
}

impl Drop for LmdbStore {
    fn drop(&mut self) {
        info!("closing DB");
    }
}

#[cfg(test)]
mod tests {
    use crate::db_store::DBStore;

    use super::{LmdbStore, WriteBatch, CURRENT_FORMAT};

    #[test]
    fn test_reindex_new_format() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = LmdbStore::open(dir.path(), false).unwrap();
            let mut config = store.get_config().unwrap();
            config.format += 1;
            store.set_config(config);
        };
        assert_eq!(
            LmdbStore::open(dir.path(), false)
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
            let store = LmdbStore::open(dir.path(), true).unwrap();
            store.flush();
            let config = store.get_config().unwrap();
            assert_eq!(config.format, CURRENT_FORMAT);
        }
    }

    #[test]
    fn test_db_prefix_scan() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), true).unwrap();

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
