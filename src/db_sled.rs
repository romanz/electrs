use std::fs::remove_dir_all;
use std::iter;
use std::path::Path;

use anyhow::{Context, Result};

use crate::db_store::{
    Config, DBStore, Row, WriteBatch, CONFIG_GROUP, CONFIG_KEY, CURRENT_FORMAT, FUNDING_GROUP,
    HEADERS_GROUP, SPENDING_GROUP, TIP_KEY, TXID_GROUP,
};

/// sled wrapper for index storage
pub struct SledStore {
    config_tree: sled::Tree,
    funding_tree: sled::Tree,
    spending_tree: sled::Tree,
    txid_tree: sled::Tree,
    headers_tree: sled::Tree,
}

fn default_db_config(path: &Path) -> sled::Config {
    sled::Config::default()
        .path(path)
        .cache_capacity(4 * 1024 * 1024)
        .mode(sled::Mode::HighThroughput)
        .use_compression(true)
        .compression_factor(11)
        .temporary(false)
        .flush_every_ms(Some(1000))
}

impl SledStore {
    fn open_internal(path: &Path) -> Result<Self> {
        let db_config = default_db_config(path);

        let db = db_config
            .open()
            .with_context(|| format!("failed to open DB: {}", path.display()))?;
        info!(
            "{:?}: {} Trees, {} GB",
            path,
            db.tree_names().len(),
            db.size_on_disk().unwrap_or_default()
        );

        let config_tree = db.open_tree(CONFIG_GROUP).expect("missing CONFIG_TREE");
        let funding_tree = db.open_tree(FUNDING_GROUP).expect("missing FUNDING_TREE");
        let spending_tree = db.open_tree(SPENDING_GROUP).expect("missing SPENDING_TREE");
        let txid_tree = db.open_tree(TXID_GROUP).expect("missing TXID_REE");
        let headers_tree = db.open_tree(HEADERS_GROUP).expect("missing HEADERS_TREE");

        let store = SledStore {
            config_tree,
            funding_tree,
            spending_tree,
            txid_tree,
            headers_tree,
        };
        Ok(store)
    }

    fn iter_prefix_tree(&self, tree: &sled::Tree, prefix: Row) -> impl Iterator<Item = Row> + '_ {
        let iter = tree.scan_prefix::<Row>(prefix);
        iter.keys().map(|key| Row::from(&key.unwrap()[..]))
    }

    fn set_config(&self, config: Config) {
        let value = serde_json::to_vec(&config).expect("failed to serialize config");
        self.config_tree
            .insert(CONFIG_KEY, value)
            .expect("DB::put failed");
    }

    fn get_config(&self) -> Option<Config> {
        self.config_tree
            .get(CONFIG_KEY)
            .expect("DB::get failed")
            .map(|value| serde_json::from_slice(&value).expect("failed to deserialize Config"))
    }
}

impl DBStore for SledStore {
    /// Opens a new sled DB at the specified location.
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
        Box::new(self.iter_prefix_tree(&self.funding_tree, prefix))
    }

    fn iter_spending(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_> {
        Box::new(self.iter_prefix_tree(&self.spending_tree, prefix))
    }

    fn iter_txid(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_> {
        Box::new(self.iter_prefix_tree(&self.txid_tree, prefix))
    }

    fn read_headers(&self) -> Vec<Row> {
        let iter = self.headers_tree.iter();
        iter.keys()
            .map(|key| Row::from(&key.unwrap()[..]))
            .filter(|key| &key[..] != TIP_KEY) // headers' rows are longer than TIP_KEY
            .collect()
    }

    fn get_tip(&self) -> Option<Vec<u8>> {
        self.headers_tree
            .get(TIP_KEY)
            .expect("get_tip failed")
            .map(|tip| tip.to_vec())
    }

    fn write(&self, batch: &WriteBatch) {
        for key in &batch.funding_rows {
            self.funding_tree
                .insert(&key[..], b"")
                .expect("Tree::insert failed");
        }

        for key in &batch.spending_rows {
            self.spending_tree
                .insert(&key[..], b"")
                .expect("Tree::insert failed");
        }

        for key in &batch.txid_rows {
            self.txid_tree
                .insert(&key[..], b"")
                .expect("Tree::insert failed");
        }

        for key in &batch.header_rows {
            self.headers_tree
                .insert(&key[..], b"")
                .expect("Tree::insert failed");
        }
        let key = TIP_KEY;
        let value = &(*batch.tip_row);
        self.headers_tree
            .insert(key, value)
            .expect("Tree::insert failed");
    }

    fn flush(&self) {
        self.config_tree.flush().expect("CONFIG_TREE flush failed");
        self.funding_tree
            .flush()
            .expect("FUNDING_TREE flush failed");
        self.spending_tree
            .flush()
            .expect("SPENDING_TREE flush failed");
        self.txid_tree.flush().expect("TXID_TREE flush failed");
        self.headers_tree
            .flush()
            .expect("HEADERS_TREE flush failed");
    }

    fn get_properties(&self) -> Box<dyn Iterator<Item = (&'static str, &'static str, u64)> + '_> {
        Box::new(iter::empty::<(&'static str, &'static str, u64)>())
    }
}

impl Drop for SledStore {
    fn drop(&mut self) {
        info!("closing DB");
    }
}

#[cfg(test)]
mod tests {
    use crate::db_store::DBStore;

    use super::{SledStore, WriteBatch, CURRENT_FORMAT};

    #[test]
    fn test_reindex_new_format() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = SledStore::open(dir.path(), false).unwrap();
            let mut config = store.get_config().unwrap();
            config.format += 1;
            store.set_config(config);
        };
        assert_eq!(
            SledStore::open(dir.path(), false)
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
            let store = SledStore::open(dir.path(), true).unwrap();
            store.flush();
            let config = store.get_config().unwrap();
            assert_eq!(config.format, CURRENT_FORMAT);
        }
    }

    #[test]
    fn test_db_prefix_scan() {
        let dir = tempfile::tempdir().unwrap();
        let store = SledStore::open(dir.path(), true).unwrap();

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
