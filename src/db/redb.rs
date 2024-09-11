use super::{hash_prefix_range, Database, WriteBatch};
use crate::types::{
    HashPrefix, SerializedHashPrefixRow, SerializedHeaderRow, HASH_PREFIX_ROW_SIZE, HEADER_ROW_SIZE,
};
use anyhow::{Context, Result};
use redb::{ReadableTableMetadata, TableDefinition, TableHandle};
use std::fs::{create_dir_all, remove_file};
use std::path::Path;

const CONFIG_TABLE: TableDefinition<u8, &[u8]> = TableDefinition::new("config");
const HEADERS_TABLE: TableDefinition<SerializedHeaderRow, ()> = TableDefinition::new("headers");
const FUNDING_TABLE: TableDefinition<SerializedHashPrefixRow, ()> = TableDefinition::new("funding");
const SPENDING_TABLE: TableDefinition<SerializedHashPrefixRow, ()> =
    TableDefinition::new("spending");
const TXID_TABLE: TableDefinition<SerializedHashPrefixRow, ()> = TableDefinition::new("txid");

const FORMAT_KEY: u8 = b'F';
const TIP_KEY: u8 = b'T';

const CURRENT_FORMAT: u64 = 0;

pub struct RowKeyIter<'a, const N: usize>(redb::Range<'a, [u8; N], ()>);

impl<const N: usize> Iterator for RowKeyIter<'_, N> {
    type Item = [u8; N];

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|v| v.unwrap().0.value())
    }
}

pub struct DBStore {
    db: redb::Database,
}

impl Database for DBStore {
    fn open(
        path: &Path,
        log_dir: Option<&Path>,
        auto_reindex: bool,
        _db_parallelism: u8,
    ) -> Result<Self> {
        create_dir_all(path).expect("unable to create directories");

        if log_dir.is_some() {
            info!("ignoring log_dir parameter, it is currently unsupported with redb");
        }

        let path = path.join("electrs.redb");
        let mut this = Self::open_internal(&path)?;

        fn create_tables(db: &redb::Database) -> Result<(), redb::Error> {
            let write_txn = db.begin_write()?;

            write_txn.open_table(FUNDING_TABLE)?;
            write_txn.open_table(SPENDING_TABLE)?;
            write_txn.open_table(TXID_TABLE)?;
            write_txn.open_table(HEADERS_TABLE)?;
            write_txn.open_table(CONFIG_TABLE)?;

            write_txn.commit()?;

            Ok(())
        }

        create_tables(&this.db).expect("unable to create tables");

        let reindex_cause = if let Some(v) = this.config_value(FORMAT_KEY).expect("redb read error")
        {
            let format = u64::from_be_bytes(v);
            if format != CURRENT_FORMAT {
                Some(format!(
                    "unsupported format {} != {}",
                    format, CURRENT_FORMAT
                ))
            } else {
                None
            }
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
            drop(this);
            remove_file(&path).with_context(|| {
                format!(
                    "re-index required but the old database ({}) can not be deleted",
                    path.display()
                )
            })?;
            this = Self::open_internal(&path)?;
        }

        this.set_config_value(FORMAT_KEY, CURRENT_FORMAT.to_be_bytes())
            .expect("redb write error");
        Ok(this)
    }

    type HashPrefixRowIter<'a> = RowKeyIter<'a, HASH_PREFIX_ROW_SIZE>;

    fn iter_funding(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_> {
        self.iter_table_hash_prefix(FUNDING_TABLE, prefix)
    }

    fn iter_spending(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_> {
        self.iter_table_hash_prefix(SPENDING_TABLE, prefix)
    }

    fn iter_txid(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_> {
        self.iter_table_hash_prefix(TXID_TABLE, prefix)
    }

    type HeaderIter<'a> = RowKeyIter<'a, HEADER_ROW_SIZE>;

    fn iter_headers(&self) -> Self::HeaderIter<'_> {
        let read_txn = self
            .db
            .begin_read()
            .expect("unable to create read transaction");
        let table = read_txn
            .open_table(HEADERS_TABLE)
            .expect("unable to open table");

        RowKeyIter(
            table
                .range::<SerializedHeaderRow>(..)
                .expect("unable to create range iterator"),
        )
    }

    fn get_tip(&self) -> Option<Vec<u8>> {
        self.config_value(TIP_KEY)
            .expect("unable to get tip")
            .map(|arr: [u8; 32]| arr.to_vec())
    }

    fn write(&self, batch: &WriteBatch) {
        fn write_return_error(db: &redb::Database, batch: &WriteBatch) -> Result<(), redb::Error> {
            let write_txn = db.begin_write()?;
            // TODO try this
            // write_txn.set_durability(redb::Durability::None);
            {
                let mut table = write_txn.open_table(FUNDING_TABLE)?;
                for key in &batch.funding_rows {
                    table.insert(key, ())?;
                }

                let mut table = write_txn.open_table(SPENDING_TABLE)?;
                for key in &batch.spending_rows {
                    table.insert(key, ())?;
                }

                let mut table = write_txn.open_table(TXID_TABLE)?;
                for key in &batch.txid_rows {
                    table.insert(key, ())?;
                }

                let mut table = write_txn.open_table(HEADERS_TABLE)?;
                for key in &batch.header_rows {
                    table.insert(key, ())?;
                }

                let mut table = write_txn.open_table(CONFIG_TABLE)?;
                table.insert(TIP_KEY, &batch.tip_row as &[u8])?;
            }

            write_txn.commit()?;

            Ok(())
        }

        write_return_error(&self.db, batch).expect("redb write error");
    }

    fn flush(&self) {
        let write_txn = self
            .db
            .begin_write()
            .expect("failed to begin write transaction");
        write_txn.commit().expect("failed to commit transaction");
    }

    fn update_metrics(&self, gauge: &crate::metrics::Gauge) {
        fn update_table_metrics<K: redb::Key, V: redb::Value>(
            gauge: &crate::metrics::Gauge,
            read_txn: &redb::ReadTransaction,
            table_definition: redb::TableDefinition<K, V>,
        ) {
            let table = read_txn
                .open_table(table_definition)
                .expect("unable to open table");

            let table_name = table_definition.name();

            let stats = table.stats().expect("unable to get table stats");

            for (name, value) in [
                ("tree_height", stats.tree_height() as f64),
                ("leaf_pages", stats.leaf_pages() as f64),
                ("branch_pages", stats.branch_pages() as f64),
                ("stored_leaf_bytes", stats.stored_bytes() as f64),
                ("metadata_bytes", stats.metadata_bytes() as f64),
                ("fragmented_bytes", stats.fragmented_bytes() as f64),
            ] {
                gauge.set(&format!("redb.{}:{}", name, table_name), value);
            }
        }

        let read_txn = self
            .db
            .begin_read()
            .expect("unable to create read transaction");

        update_table_metrics(gauge, &read_txn, FUNDING_TABLE);
        update_table_metrics(gauge, &read_txn, SPENDING_TABLE);
        update_table_metrics(gauge, &read_txn, TXID_TABLE);
        update_table_metrics(gauge, &read_txn, HEADERS_TABLE);
        update_table_metrics(gauge, &read_txn, CONFIG_TABLE);
    }
}

impl DBStore {
    fn open_internal(path: &Path) -> Result<Self, redb::Error> {
        const GIGABYTE: usize = 1024 * 1024 * 1024;

        Ok(Self {
            db: redb::Database::builder()
                .set_cache_size(4 * GIGABYTE)
                .create(path)?,
        })
    }

    fn iter_table_hash_prefix(
        &self,
        table_definition: TableDefinition<SerializedHashPrefixRow, ()>,
        prefix: HashPrefix,
    ) -> RowKeyIter<'_, HASH_PREFIX_ROW_SIZE> {
        let read_txn = self
            .db
            .begin_read()
            .expect("unable to create read transaction");
        let table = read_txn
            .open_table(table_definition)
            .expect("unable to open table");

        RowKeyIter(
            table
                .range(hash_prefix_range(prefix))
                .expect("unable to create range iterator"),
        )
    }

    fn config_value<const N: usize>(&self, key: u8) -> Result<Option<[u8; N]>, redb::Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CONFIG_TABLE)?;
        Ok(table.get(key)?.map(|v| v.value().try_into().unwrap()))
    }

    fn set_config_value<const N: usize>(&self, key: u8, value: [u8; N]) -> Result<(), redb::Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(CONFIG_TABLE)?;
            table.insert(key, &value as &[u8])?;
        }
        write_txn.commit()?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::DBStore;

    #[test]
    fn test_db_prefix_scan() {
        super::super::test_db_prefix_scan::<DBStore>();
    }
}
