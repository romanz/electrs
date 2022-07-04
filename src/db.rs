extern crate sled;

use anyhow::{Context, Result};
use sled::Transactional;

use std::fs::remove_dir_all;
use std::iter;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) type Row = Box<[u8]>;

#[derive(Default)]
pub(crate) struct WriteBatch {
    pub(crate) tip_row: Row,
    pub(crate) header_rows: Vec<Row>,
    pub(crate) funding_rows: Vec<Row>,
    pub(crate) spending_rows: Vec<Row>,
    pub(crate) txid_rows: Vec<Row>,
}

impl WriteBatch {
    pub(crate) fn sort(&mut self) {
        self.header_rows.sort_unstable();
        self.funding_rows.sort_unstable();
        self.spending_rows.sort_unstable();
        self.txid_rows.sort_unstable();
    }
}

/// sled wrapper for index storage
pub struct DBStore {
    db: sled::Db,
    bulk_import: AtomicBool,
}

const CONFIG_TREE: &str = "config";
const HEADERS_TREE: &str = "headers";
const TXID_TREE: &str = "txid";
const FUNDING_TREE: &str = "funding";
const SPENDING_TREE: &str = "spending";

const TREES: &[&str] = &[
    CONFIG_TREE,
    HEADERS_TREE,
    TXID_TREE,
    FUNDING_TREE,
    SPENDING_TREE,
];

const CONFIG_KEY: &str = "C";
const TIP_KEY: &[u8] = b"T";

#[derive(Debug, Deserialize, Serialize)]
struct Config {
    compacted: bool,
    format: u64,
}

const CURRENT_FORMAT: u64 = 0;

impl Default for Config {
    fn default() -> Self {
        Config {
            compacted: false,
            format: CURRENT_FORMAT,
        }
    }
}

fn default_db_config(path: &Path) -> sled::Config {
    sled::Config::default()
        .path(path)
        .cache_capacity(10_000_000_000)
        .mode(sled::Mode::HighThroughput)
        .use_compression(false)
        .compression_factor(11)
        .temporary(false)
        .flush_every_ms(Some(1000))
}

impl DBStore {
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
        let store = DBStore {
            db,
            bulk_import: AtomicBool::new(true),
        };
        Ok(store)
    }

    /// Opens a new sled DB at the specified location.
    pub fn open(path: &Path, auto_reindex: bool) -> Result<Self> {
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
            let tree_names = store.db.tree_names();
            for name in tree_names.iter() {
                store.db.drop_tree(name).with_context(|| {
                    format!(
                        "re-index required but the old database ({}) can not be deleted",
                        path.display()
                    )
                })?;
            }
            drop(store);
            remove_dir_all(path).unwrap();
            store = Self::open_internal(path)?;
            config = Config::default();
        }
        store.set_config(config);
        Ok(store)
    }

    fn config_tree(&self) -> sled::Tree {
        self.db.open_tree(CONFIG_TREE).expect("missing CONFIG_TREE")
    }

    fn funding_tree(&self) -> sled::Tree {
        self.db
            .open_tree(FUNDING_TREE)
            .expect("missing FUNDING_TREE")
    }

    fn spending_tree(&self) -> sled::Tree {
        self.db
            .open_tree(SPENDING_TREE)
            .expect("missing SPENDING_TREE")
    }

    fn txid_tree(&self) -> sled::Tree {
        self.db.open_tree(TXID_TREE).expect("missing TXID_TREE")
    }

    fn headers_tree(&self) -> sled::Tree {
        self.db
            .open_tree(HEADERS_TREE)
            .expect("missing HEADERS_TREE")
    }

    pub(crate) fn iter_funding(&self, prefix: Row) -> impl Iterator<Item = Row> + '_ {
        self.iter_prefix_tree(&self.funding_tree(), prefix)
    }

    pub(crate) fn iter_spending(&self, prefix: Row) -> impl Iterator<Item = Row> + '_ {
        self.iter_prefix_tree(&self.spending_tree(), prefix)
    }

    pub(crate) fn iter_txid(&self, prefix: Row) -> impl Iterator<Item = Row> + '_ {
        self.iter_prefix_tree(&self.txid_tree(), prefix)
    }

    fn iter_prefix_tree(&self, tree: &sled::Tree, prefix: Row) -> impl Iterator<Item = Row> + '_ {
        let iter = tree.scan_prefix::<Row>(prefix);
        iter.keys().map(|key| Row::from(&key.unwrap()[..]))
    }

    pub(crate) fn read_headers(&self) -> Vec<Row> {
        let iter = self.headers_tree().iter();
        iter.keys()
            .map(|key| Row::from(&key.unwrap()[..]))
            .filter(|key| &key[..] != TIP_KEY) // headers' rows are longer than TIP_KEY
            .collect()
    }

    pub(crate) fn get_tip(&self) -> Option<Vec<u8>> {
        let res = self.headers_tree().get(TIP_KEY).expect("get_tip failed");

        if let Some(tip) = res {
            Some(tip.to_vec())
        } else {
            None
        }
    }

    pub(crate) fn write(&self, batch: &WriteBatch) {
        let _res: sled::transaction::TransactionError<()> = (
            &self.funding_tree(),
            &self.spending_tree(),
            &self.txid_tree(),
            &self.headers_tree(),
        )
            .transaction(|(funding_tree, spending_tree, txid_tree, headers_tree)| {
                let mut db_batch = sled::Batch::default();
                for key in &batch.funding_rows {
                    db_batch.insert(&key[..], b"");
                }
                funding_tree.apply_batch(&db_batch)?;

                let mut db_batch = sled::Batch::default();
                for key in &batch.spending_rows {
                    db_batch.insert(&key[..], b"");
                }
                spending_tree.apply_batch(&db_batch)?;

                let mut db_batch = sled::Batch::default();
                for key in &batch.txid_rows {
                    db_batch.insert(&key[..], b"");
                }
                txid_tree.apply_batch(&db_batch)?;

                let mut db_batch = sled::Batch::default();
                for key in &batch.header_rows {
                    db_batch.insert(&key[..], b"");
                }
                let key = TIP_KEY;
                let value = &(*batch.tip_row);
                db_batch.insert(key, value);
                headers_tree.apply_batch(&db_batch)?;

                Ok(())
            })
            .unwrap_err();
    }

    pub(crate) fn flush(&self) {
        for name in TREES {
            let tree = self.db.open_tree(name).expect("missing TREE");
            let _bulk_import = self.bulk_import.load(Ordering::Relaxed);

            tree.flush().expect("TREE flush failed");
        }
    }

    pub(crate) fn get_properties(
        &self,
    ) -> impl Iterator<Item = (&'static str, &'static str, u64)> + '_ {
        iter::empty::<(&'static str, &'static str, u64)>()
    }

    fn set_config(&self, config: Config) {
        let value = serde_json::to_vec(&config).expect("failed to serialize config");
        self.config_tree()
            .insert(CONFIG_KEY, value)
            .expect("DB::put failed");
    }

    fn get_config(&self) -> Option<Config> {
        self.config_tree()
            .get(CONFIG_KEY)
            .expect("DB::get failed")
            .map(|value| serde_json::from_slice(&value).expect("failed to deserialize Config"))
    }
}

impl Drop for DBStore {
    fn drop(&mut self) {
        info!("closing DB");
    }
}

#[cfg(test)]
mod tests {
    use super::{DBStore, WriteBatch, CURRENT_FORMAT};

    #[test]
    fn test_reindex_new_format() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = DBStore::open(dir.path(), false).unwrap();
            let mut config = store.get_config().unwrap();
            config.format += 1;
            store.set_config(config);
        };
        assert_eq!(
            DBStore::open(dir.path(), false).err().unwrap().to_string(),
            format!(
                "re-index required due to unsupported format {} != {}",
                CURRENT_FORMAT + 1,
                CURRENT_FORMAT
            )
        );
        {
            let store = DBStore::open(dir.path(), true).unwrap();
            store.flush();
            let config = store.get_config().unwrap();
            assert_eq!(config.format, CURRENT_FORMAT);
        }
    }

    #[test]
    fn test_db_prefix_scan() {
        let dir = tempfile::tempdir().unwrap();
        let store = DBStore::open(dir.path(), true).unwrap();

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
