use std::{
    fs::{create_dir_all, remove_file},
    path::Path,
};

use anyhow::{Context, Result};
use heed::{
    types::{Bytes, Unit, U8},
    EnvOpenOptions, RoTxn,
};

use crate::types::{HashPrefix, SerializedHeaderRow, HASH_PREFIX_ROW_SIZE};

use super::{Database, WriteBatch};

const FORMAT_KEY: u8 = b'F';
const TIP_KEY: u8 = b'T';

const CURRENT_FORMAT: u64 = 0;

// TODO fix
// field 0 is used in the generated drop code
#[allow(dead_code)]
pub struct RowKeyIter<'a, const N: usize>(
    RoTxn<'static, heed::WithTls>,
    heed::RoPrefix<'a, Bytes, Unit>,
);

impl<const N: usize> Iterator for RowKeyIter<'_, N> {
    type Item = [u8; N];

    fn next(&mut self) -> Option<Self::Item> {
        self.1.next().map(|v| v.unwrap().0.try_into().unwrap())
    }
}

#[allow(dead_code)]
pub struct HeaderIter<'a>(RoTxn<'static, heed::WithTls>, heed::RoIter<'a, Bytes, Unit>);

impl Iterator for HeaderIter<'_> {
    type Item = SerializedHeaderRow;

    fn next(&mut self) -> Option<Self::Item> {
        self.1.next().map(|v| v.unwrap().0.try_into().unwrap())
    }
}

pub struct DBStore {
    env: heed::Env,
    config: heed::Database<U8, Bytes>,
    headers: heed::Database<Bytes, Unit>,
    funding: heed::Database<Bytes, Unit>,
    spending: heed::Database<Bytes, Unit>,
    txid: heed::Database<Bytes, Unit>,
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
            info!("ignoring log_dir parameter, it is currently unsupported with lmdb");
        }

        let mut this = Self::open_internal(path)?;

        let reindex_cause = if let Some(v) = this.config_value(FORMAT_KEY).expect("lmdb read error")
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
            remove_file(path).with_context(|| {
                format!(
                    "re-index required but the old database ({}) can not be deleted",
                    path.display()
                )
            })?;
            this = Self::open_internal(path)?;
        }

        this.set_config_value(FORMAT_KEY, CURRENT_FORMAT.to_be_bytes())
            .expect("lmdb write error");
        Ok(this)
    }

    type HashPrefixRowIter<'a> = RowKeyIter<'a, HASH_PREFIX_ROW_SIZE>;

    fn iter_funding(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_> {
        self.iter_table_hash_prefix(&self.funding, prefix)
    }

    fn iter_spending(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_> {
        self.iter_table_hash_prefix(&self.spending, prefix)
    }

    fn iter_txid(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_> {
        self.iter_table_hash_prefix(&self.txid, prefix)
    }

    type HeaderIter<'a> = HeaderIter<'a>;

    fn iter_headers(&self) -> Self::HeaderIter<'_> {
        let rtxn = self.env.clone().static_read_txn().unwrap();

        let iter = self
            .headers
            .iter(
                // TODO fix
                unsafe { &*(&rtxn as *const RoTxn<heed::WithTls>) },
            )
            .unwrap();

        HeaderIter(rtxn, iter)
    }

    fn get_tip(&self) -> Option<Vec<u8>> {
        self.config_value(TIP_KEY)
            .expect("unable to get tip")
            .map(|arr: [u8; 32]| arr.to_vec())
    }

    fn write(&self, batch: &WriteBatch) {
        fn write_return_error(this: &DBStore, batch: &WriteBatch) -> Result<(), heed::Error> {
            let mut wtxn = this.env.write_txn()?;

            for key in &batch.funding_rows {
                this.funding.put(&mut wtxn, key, &())?;
            }

            for key in &batch.spending_rows {
                this.spending.put(&mut wtxn, key, &())?;
            }

            for key in &batch.txid_rows {
                this.txid.put(&mut wtxn, key, &())?;
            }

            for key in &batch.header_rows {
                this.headers.put(&mut wtxn, key, &())?;
            }

            this.config.put(&mut wtxn, &TIP_KEY, &batch.tip_row)?;

            wtxn.commit()?;

            Ok(())
        }

        write_return_error(self, batch).expect("lmdb write error");
    }

    fn flush(&self) {
        self.env.force_sync().unwrap();
    }

    fn update_metrics(&self, gauge: &crate::metrics::Gauge) {
        fn update_table_metrics<KC, DC>(
            gauge: &crate::metrics::Gauge,
            rtxn: &RoTxn,
            db: &heed::Database<KC, DC>,
            db_name: &str,
        ) {
            let stats = db.stat(rtxn).unwrap();

            for (name, value) in [
                ("page_size", stats.page_size as f64),
                ("depth", stats.depth as f64),
                ("branch_pages", stats.branch_pages as f64),
                ("leaf_pages", stats.leaf_pages as f64),
                ("overflow_pages", stats.overflow_pages as f64),
                ("entries", stats.entries as f64),
            ] {
                gauge.set(&format!("lmdb.{}:{}", name, db_name), value);
            }
        }

        gauge.set(
            "lmdb.size_on_disk",
            self.env
                .real_disk_size()
                .expect("unable to get database size") as f64,
        );

        let rtxn = self.env.read_txn().unwrap();

        update_table_metrics(gauge, &rtxn, &self.funding, "funding");
        update_table_metrics(gauge, &rtxn, &self.spending, "spending");
        update_table_metrics(gauge, &rtxn, &self.txid, "txid");
        update_table_metrics(gauge, &rtxn, &self.headers, "headers");
        update_table_metrics(gauge, &rtxn, &self.config, "config");
    }
}

impl DBStore {
    fn open_internal(path: &Path) -> Result<Self, heed::Error> {
        let env = unsafe {
            EnvOpenOptions::new()
                .map_size(1024 * 1024 * 1024 * 1024)
                .max_dbs(5)
                .open(path)?
        };

        let mut wtxn = env.write_txn()?;

        let config = env.create_database(&mut wtxn, Some("config"))?;
        let headers = env.create_database(&mut wtxn, Some("headers"))?;
        let funding = env.create_database(&mut wtxn, Some("funding"))?;
        let spending = env.create_database(&mut wtxn, Some("spending"))?;
        let txid = env.create_database(&mut wtxn, Some("txid"))?;

        wtxn.commit()?;

        Ok(Self {
            env,
            config,
            headers,
            funding,
            spending,
            txid,
        })
    }

    fn iter_table_hash_prefix(
        &self,
        db: &heed::Database<Bytes, Unit>,
        prefix: HashPrefix,
    ) -> RowKeyIter<'_, HASH_PREFIX_ROW_SIZE> {
        let rtxn = self.env.clone().static_read_txn().unwrap();

        let iter = db
            .prefix_iter(
                // TODO fix
                unsafe { &*(&rtxn as *const RoTxn<heed::WithTls>) },
                &prefix,
            )
            .unwrap();

        RowKeyIter(rtxn, iter)
    }

    fn config_value<const N: usize>(&self, key: u8) -> Result<Option<[u8; N]>, heed::Error> {
        let rtxn = self.env.read_txn()?;

        Ok(self.config.get(&rtxn, &key)?.map(|v| v.try_into().unwrap()))
    }

    fn set_config_value<const N: usize>(&self, key: u8, value: [u8; N]) -> Result<(), heed::Error> {
        let mut wtxn = self.env.write_txn()?;
        self.config.put(&mut wtxn, &key, &value)?;
        wtxn.commit()?;

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
