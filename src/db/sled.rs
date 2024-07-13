use std::fs::{create_dir_all, remove_dir_all};
use std::path::Path;

use anyhow::{Context, Result};
use sled::Batch;

use super::{hash_prefix_range, Database, WriteBatch};

use crate::types::{HashPrefix, SerializedHeaderRow, HASH_PREFIX_ROW_SIZE, HEADER_ROW_SIZE};

const FORMAT_KEY: u8 = b'F';
const TIP_KEY: u8 = b'T';

const CURRENT_FORMAT: u64 = 0;

pub struct RowKeyIter<const N: usize>(sled::Iter);

impl<const N: usize> Iterator for RowKeyIter<N> {
    type Item = [u8; N];

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|v| (&*v.unwrap().0).try_into().unwrap())
    }
}

pub struct DBStore {
    db: sled::Db,
    config: sled::Tree,
    headers: sled::Tree,
    funding: sled::Tree,
    spending: sled::Tree,
    txid: sled::Tree,
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
            info!("ignoring log_dir parameter, it is currently unsupported with sled");
        }

        let mut this = Self::open_internal(path)?;

        let format = this
            .config_value(FORMAT_KEY)?
            .map(u64::from_be_bytes)
            .unwrap_or(CURRENT_FORMAT);
        let reindex_cause = if format != CURRENT_FORMAT {
            Some(format!(
                "unsupported format {} != {}",
                format, CURRENT_FORMAT
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
            drop(this);
            remove_dir_all(path).with_context(|| {
                format!(
                    "re-index required but the old database ({}) can not be deleted",
                    path.display()
                )
            })?;
            this = Self::open_internal(path)?;
        }

        this.set_config_value(FORMAT_KEY, format.to_be_bytes())
            .expect("unable to write format");

        Ok(this)
    }

    type HashPrefixRowIter<'a> = RowKeyIter<HASH_PREFIX_ROW_SIZE>;

    fn iter_funding(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_> {
        RowKeyIter(self.funding.range(hash_prefix_range(prefix)))
    }

    fn iter_spending(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_> {
        RowKeyIter(self.spending.range(hash_prefix_range(prefix)))
    }

    fn iter_txid(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_> {
        RowKeyIter(self.txid.range(hash_prefix_range(prefix)))
    }

    type HeaderIter<'a> = RowKeyIter<HEADER_ROW_SIZE>;

    fn iter_headers(&self) -> Self::HeaderIter<'_> {
        RowKeyIter(self.headers.range::<SerializedHeaderRow, _>(..))
    }

    fn get_tip(&self) -> Option<Vec<u8>> {
        self.config_value(TIP_KEY)
            .expect("unable to read tip")
            .map(|t: [u8; 32]| t.to_vec())
    }

    fn write(&self, batch: &WriteBatch) {
        // TODO associated type to not have to transfer all data from WriteBatch to sled::Batch

        {
            let mut headers_batch = Batch::default();
            for key in &batch.header_rows {
                headers_batch.insert(key as &[u8], &[]);
            }

            self.headers
                .apply_batch(headers_batch)
                .expect("sled write error");
        }

        {
            let mut funding_batch = Batch::default();
            for key in &batch.funding_rows {
                funding_batch.insert(key, &[]);
            }

            self.funding
                .apply_batch(funding_batch)
                .expect("sled write error");
        }

        {
            let mut spending_batch = Batch::default();
            for key in &batch.spending_rows {
                spending_batch.insert(key, &[]);
            }

            self.spending
                .apply_batch(spending_batch)
                .expect("sled write error");
        }

        {
            let mut txid_batch = Batch::default();
            for key in &batch.txid_rows {
                txid_batch.insert(key, &[]);
            }

            self.txid.apply_batch(txid_batch).expect("sled write error");
        }

        self.set_config_value(TIP_KEY, batch.tip_row)
            .expect("unable to write tip");
    }

    fn flush(&self) {
        self.db.flush().expect("flush failed");
    }

    fn update_metrics(&self, gauge: &crate::metrics::Gauge) {
        gauge.set(
            "sled.size_on_disk",
            self.db.size_on_disk().expect("unable to get database size") as f64,
        );
    }
}

impl DBStore {
    fn open_internal(path: &Path) -> Result<Self> {
        // TODO config
        let db = sled::Config::new().path(path).open()?;

        let config = db.open_tree("config")?;
        let headers = db.open_tree("headers")?;
        let funding = db.open_tree("funding")?;
        let spending = db.open_tree("spending")?;
        let txid = db.open_tree("txid")?;

        Ok(Self {
            db,
            config,
            headers,
            funding,
            spending,
            txid,
        })
    }

    fn config_value<const N: usize>(&self, key: u8) -> Result<Option<[u8; N]>, sled::Error> {
        Ok(self.config.get([key])?.map(|v| (&*v).try_into().unwrap()))
    }

    fn set_config_value<const N: usize>(&self, key: u8, value: [u8; N]) -> Result<(), sled::Error> {
        self.config.insert([key], &value as &[u8])?;
        Ok(())
    }
}
