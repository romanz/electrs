use anyhow::Result;

use std::path::Path;

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

pub(crate) trait DBStore {
    fn open(path: &Path, auto_reindex: bool) -> Result<Self>
    where
        Self: Sized;

    fn iter_funding(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_>;

    fn iter_spending(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_>;

    fn iter_txid(&self, prefix: Row) -> Box<dyn Iterator<Item = Row> + '_>;

    fn read_headers(&self) -> Vec<Row>;

    fn get_tip(&self) -> Option<Vec<u8>>;

    fn write(&self, batch: &WriteBatch);

    fn flush(&self);

    fn get_properties(&self) -> Box<dyn Iterator<Item = (&'static str, &'static str, u64)> + '_>;
}

pub(crate) const CONFIG_GROUP: &str = "config";
pub(crate) const HEADERS_GROUP: &str = "headers";
pub(crate) const TXID_GROUP: &str = "txid";
pub(crate) const FUNDING_GROUP: &str = "funding";
pub(crate) const SPENDING_GROUP: &str = "spending";

#[allow(dead_code)]
pub(crate) const GROUPS: &[&str] = &[
    CONFIG_GROUP,
    HEADERS_GROUP,
    TXID_GROUP,
    FUNDING_GROUP,
    SPENDING_GROUP,
];

pub(crate) const CONFIG_KEY: &str = "C";
pub(crate) const TIP_KEY: &[u8] = b"T";

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct Config {
    pub(crate) compacted: bool,
    pub(crate) format: u64,
}

pub(crate) const CURRENT_FORMAT: u64 = 0;

impl Default for Config {
    fn default() -> Self {
        Config {
            compacted: false,
            format: CURRENT_FORMAT,
        }
    }
}
