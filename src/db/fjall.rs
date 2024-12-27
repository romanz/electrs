use super::{Database, WriteBatch};
use crate::types::{HashPrefix, HASH_PREFIX_ROW_SIZE, HEADER_ROW_SIZE};
use anyhow::{Context, Result};
use fjall::{Keyspace, PartitionHandle, PersistMode, Slice};
use std::fs::{create_dir_all, remove_file};
use std::path::Path;

const FORMAT_KEY: u8 = b'F';
const TIP_KEY: u8 = b'T';

const CURRENT_FORMAT: u64 = 0;

pub struct RowKeyIter<const N: usize>(
    // TODO remove this (third) Box
    Box<dyn Iterator<Item = Result<(Slice, Slice), fjall::Error>>>,
);

impl<const N: usize> Iterator for RowKeyIter<N> {
    type Item = [u8; N];

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|v| (&*v.unwrap().0).try_into().unwrap())
    }
}

pub struct DBStore {
    keyspace: Keyspace,
    config: PartitionHandle,
    headers: PartitionHandle,
    funding: PartitionHandle,
    spending: PartitionHandle,
    txid: PartitionHandle,
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
            info!("ignoring log_dir parameter, it is currently unsupported with fjall");
        }

        let mut this = Self::open_internal(path)?;

        let reindex_cause =
            if let Some(v) = this.config_value(FORMAT_KEY).expect("fjall read error") {
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
            remove_file(path).with_context(|| {
                format!(
                    "re-index required but the old database ({}) can not be deleted",
                    path.display()
                )
            })?;
            this = Self::open_internal(path)?;
        }

        this.set_config_value(FORMAT_KEY, CURRENT_FORMAT.to_be_bytes())
            .expect("fjall write error");
        Ok(this)
    }

    type HashPrefixRowIter<'a> = RowKeyIter<HASH_PREFIX_ROW_SIZE>;

    fn iter_funding(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_> {
        Self::iter_table_hash_prefix(&self.funding, prefix)
    }

    fn iter_spending(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_> {
        Self::iter_table_hash_prefix(&self.spending, prefix)
    }

    fn iter_txid(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_> {
        Self::iter_table_hash_prefix(&self.txid, prefix)
    }

    type HeaderIter<'a> = RowKeyIter<HEADER_ROW_SIZE>;

    fn iter_headers(&self) -> Self::HeaderIter<'_> {
        RowKeyIter(Box::new(self.headers.iter()))
    }

    fn get_tip(&self) -> Option<Vec<u8>> {
        self.config_value(TIP_KEY)
            .expect("unable to get tip")
            .map(|arr: [u8; 32]| arr.to_vec())
    }

    fn write(&self, batch: &WriteBatch) {
        let mut txn = self.keyspace.batch();

        for key in &batch.funding_rows {
            txn.insert(&self.funding, key, []);
        }

        for key in &batch.spending_rows {
            txn.insert(&self.spending, key, []);
        }

        for key in &batch.txid_rows {
            txn.insert(&self.txid, key, []);
        }

        for key in &batch.header_rows {
            txn.insert(&self.headers, key, []);
        }

        txn.insert(&self.config, [TIP_KEY], batch.tip_row);

        txn.commit().expect("fjall write error");
    }

    fn flush(&self) {
        self.keyspace
            .persist(PersistMode::SyncAll)
            .expect("failed to flush");
    }

    fn update_metrics(&self, gauge: &crate::metrics::Gauge) {
        gauge.set("fjall.disk_space", self.keyspace.disk_space() as f64);
        gauge.set(
            "fjall.write_buffer_size",
            self.keyspace.write_buffer_size() as f64,
        );
        gauge.set("fjall.journal_count", self.keyspace.journal_count() as f64);

        for table in [
            &self.config,
            &self.headers,
            &self.funding,
            &self.spending,
            &self.txid,
        ] {
            gauge.set(
                &format!("fjall.table_disk_space:{}", table.name),
                table.disk_space() as f64,
            );
        }
    }
}

impl DBStore {
    fn open_internal(path: &Path) -> Result<Self, fjall::Error> {
        let keyspace = fjall::Config::new(path).open()?;

        let config = keyspace.open_partition("config", Default::default())?;
        let headers = keyspace.open_partition("headers", Default::default())?;
        let funding = keyspace.open_partition("funding", Default::default())?;
        let spending = keyspace.open_partition("spending", Default::default())?;
        let txid = keyspace.open_partition("txid", Default::default())?;

        Ok(Self {
            keyspace,
            config,
            headers,
            funding,
            spending,
            txid,
        })
    }

    fn iter_table_hash_prefix(
        table: &PartitionHandle,
        prefix: HashPrefix,
    ) -> RowKeyIter<HASH_PREFIX_ROW_SIZE> {
        RowKeyIter(Box::new(table.prefix(prefix)))
    }

    fn config_value<const N: usize>(&self, key: u8) -> Result<Option<[u8; N]>, fjall::Error> {
        Ok(self.config.get([key])?.map(|v| (&*v).try_into().unwrap()))
    }

    fn set_config_value<const N: usize>(
        &self,
        key: u8,
        value: [u8; N],
    ) -> Result<(), fjall::Error> {
        self.config.insert([key], value)
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
