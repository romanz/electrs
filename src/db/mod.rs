#[cfg(feature = "rocksdb")]
pub mod rocksdb;

#[cfg(feature = "redb")]
pub mod redb;

#[cfg(not(any(feature = "rocksdb", feature = "redb",)))]
compile_error!(
    "Tried to build electrs without database, but at least one is needed. \
     Enable at least one of the following features: 'rocksdb', 'redb'."
);

use anyhow::Result;

use std::ops::RangeBounds;
use std::path::Path;

use crate::types::{
    HashPrefix, SerializedHashPrefixRow, SerializedHeaderRow, HASH_PREFIX_LEN, HASH_PREFIX_ROW_SIZE,
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

pub trait Database: Sized + Sync {
    fn open(
        path: &Path,
        log_dir: Option<&Path>,
        auto_reindex: bool,
        db_parallelism: u8,
    ) -> Result<Self>;

    type HashPrefixRowIter<'a>: Iterator<Item = SerializedHashPrefixRow> + 'a
    where
        Self: 'a;

    fn iter_funding(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_>;

    fn iter_spending(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_>;

    fn iter_txid(&self, prefix: HashPrefix) -> Self::HashPrefixRowIter<'_>;

    type HeaderIter<'a>: Iterator<Item = SerializedHeaderRow> + 'a
    where
        Self: 'a;

    fn iter_headers(&self) -> Self::HeaderIter<'_>;

    fn get_tip(&self) -> Option<Vec<u8>>;

    fn write(&self, batch: &WriteBatch);

    fn flush(&self);

    fn update_metrics(&self, gauge: &crate::metrics::Gauge);
}

/// Creates a range that includes all values with the given prefix
pub(crate) fn hash_prefix_range(prefix: HashPrefix) -> impl RangeBounds<SerializedHashPrefixRow> {
    let mut lower = [0x00; HASH_PREFIX_ROW_SIZE];
    let mut upper = [0xff; HASH_PREFIX_ROW_SIZE];

    lower[..HASH_PREFIX_LEN].copy_from_slice(&prefix);
    upper[..HASH_PREFIX_LEN].copy_from_slice(&prefix);

    lower..=upper
}
