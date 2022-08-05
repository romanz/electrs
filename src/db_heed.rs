use std::fs::{create_dir_all, remove_dir_all};
use std::iter;
use std::path::Path;

use anyhow::Result;

use crate::db_store::{
    Config, DBStore, Row, WriteBatch, CONFIG_GROUP, CONFIG_KEY, CURRENT_FORMAT, FUNDING_GROUP,
    HEADERS_GROUP, SPENDING_GROUP, TIP_KEY, TXID_GROUP,
};

/// heed wrapper for index storage
pub struct HeedStore {
    env: heed::Env,
    config_db: heed::Database<heed::types::Str, heed::types::ByteSlice>,
    funding_db: heed::Database<heed::types::ByteSlice, heed::types::ByteSlice>,
    spending_db: heed::Database<heed::types::ByteSlice, heed::types::ByteSlice>,
    txid_db: heed::Database<heed::types::ByteSlice, heed::types::ByteSlice>,
    headers_db: heed::Database<heed::types::ByteSlice, heed::types::ByteSlice>,
}

fn default_env_open_options() -> heed::EnvOpenOptions {
    let mut env_open_options = heed::EnvOpenOptions::new();

    env_open_options.map_size(4 * 1024 * 1024);
    env_open_options.max_dbs(6);

    unsafe {
        env_open_options.flag(heed::flags::Flags::MdbWriteMap);
        env_open_options.flag(heed::flags::Flags::MdbNoMetaSync);
        env_open_options.flag(heed::flags::Flags::MdbNoSync);
        env_open_options.flag(heed::flags::Flags::MdbNoRdAhead);
    }

    env_open_options
}

impl HeedStore {
    fn open_internal(path: &Path) -> Result<Self> {
        create_dir_all(path.join("database.mdb"))?;
        let env_builder = default_env_open_options();

        let env = env_builder
            .open(path.join("database.mdb"))
            .expect(&format!("failed to open DB: {}", path.display()));

        let config_db = env
            .create_database(Some(CONFIG_GROUP))
            .expect("missing CONFIG_DB");
        let funding_db = env
            .create_database(Some(FUNDING_GROUP))
            .expect("missing FUNDING_DB");
        let spending_db = env
            .create_database(Some(SPENDING_GROUP))
            .expect("missing SPENDING_DB");
        let txid_db = env
            .create_database(Some(TXID_GROUP))
            .expect("missing TXID_DB");
        let headers_db = env
            .create_database(Some(HEADERS_GROUP))
            .expect("missing HEADERS_DB");

        let store = HeedStore {
            env,
            config_db,
            funding_db,
            spending_db,
            txid_db,
            headers_db,
        };
        Ok(store)
    }

    fn iter_prefix_tree(
        &self,
        db: &heed::Database<heed::types::ByteSlice, heed::types::ByteSlice>,
        prefix: Row,
    ) -> impl Iterator<Item = Row> + '_ {
        let mut rtxn = self.env.read_txn().expect("Env::read_txn failed");
        db.prefix_iter(&mut rtxn, &prefix)
            .expect("Database::prefix_iter failed")
            .map(|res| Row::from(&res.unwrap().0[..]))
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn set_config(&self, config: Config) {
        let value = serde_json::to_vec(&config).expect("failed to serialize config");

        let mut wtxn = self.env.write_txn().expect("Env::write_txn failed");
        self.config_db
            .put(&mut wtxn, CONFIG_KEY, &value)
            .expect("WriteTransaction::put failed");
        wtxn.commit().expect("WriteTransaction::commit failed");
    }

    fn get_config(&self) -> Option<Config> {
        let rtxn = self.env.read_txn().expect("Env::read_txn failed");

        let value = self
            .config_db
            .get(&rtxn, CONFIG_KEY)
            .expect("ReadTransaction::get failed")
            .expect("ReadTransaction::get failed");
        serde_json::from_slice(value).expect("failed to deserialize Config")
    }
}

impl DBStore for HeedStore {
    /// Opens a new heed DB at the specified location.
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
        Box::new(self.iter_prefix_tree(&self.funding_db, prefix))
    }

    fn iter_spending(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_> {
        Box::new(self.iter_prefix_tree(&self.spending_db, prefix))
    }

    fn iter_txid(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_> {
        Box::new(self.iter_prefix_tree(&self.txid_db, prefix))
    }

    fn read_headers(&self) -> Vec<Row> {
        let mut rtxn = self.env.read_txn().expect("Env::read_txn failed");
        self.headers_db
            .iter(&mut rtxn)
            .expect("Database::iter failed")
            .map(|res| Row::from(&res.unwrap().0[..]))
            .filter(|key| &key[..] != TIP_KEY) // headers' rows are longer than TIP_KEY
            .collect()
    }

    fn get_tip(&self) -> Option<Vec<u8>> {
        let rtxn = self.env.read_txn().expect("Env::read_txn failed");

        self.headers_db
            .get(&rtxn, TIP_KEY)
            .expect("ReadTransaction::get failed")
            .map(|tip| tip.to_vec())
    }

    fn write(&self, batch: &WriteBatch) {
        let mut wtxn = self.env.write_txn().expect("Env::write_txn failed");

        for key in &batch.funding_rows {
            self.funding_db
                .put(&mut wtxn, key, b"")
                .expect("Database::put failed");
        }

        for key in &batch.spending_rows {
            self.spending_db
                .put(&mut wtxn, key, b"")
                .expect("Database::put failed");
        }

        for key in &batch.txid_rows {
            self.txid_db
                .put(&mut wtxn, key, b"")
                .expect("Database::put failed");
        }

        for key in &batch.header_rows {
            self.headers_db
                .put(&mut wtxn, key, b"")
                .expect("Database::put failed");
        }
        self.headers_db
            .put(&mut wtxn, TIP_KEY, &batch.tip_row)
            .expect("Database::put failed");

        wtxn.commit().expect("WriteTransaction::commit failed");
    }

    fn flush(&self) {
        self.env.force_sync().expect("Env::Synv failed");
    }

    fn get_properties(&self) -> Box<dyn Iterator<Item = (&'static str, &'static str, u64)> + '_> {
        Box::new(iter::empty::<(&'static str, &'static str, u64)>())
    }
}

impl Drop for HeedStore {
    fn drop(&mut self) {
        info!("closing DB");
    }
}

#[cfg(test)]
mod tests {
    use crate::db_store::DBStore;

    use super::{HeedStore, WriteBatch, CURRENT_FORMAT};

    #[test]
    fn test_reindex_new_format() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = HeedStore::open(dir.path(), false).unwrap();
            let mut config = store.get_config().unwrap();
            config.format += 1;
            store.set_config(config);
        };
        assert_eq!(
            HeedStore::open(dir.path(), false)
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
            let store = HeedStore::open(dir.path(), true).unwrap();
            store.flush();
            let config = store.get_config().unwrap();
            assert_eq!(config.format, CURRENT_FORMAT);
        }
    }

    #[test]
    fn test_db_prefix_scan() {
        let dir = tempfile::tempdir().unwrap();
        let store = HeedStore::open(dir.path(), true).unwrap();

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
