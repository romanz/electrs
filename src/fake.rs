use store::{ReadStore, Row, WriteStore};
use util::Bytes;

pub struct FakeStore;

impl ReadStore for FakeStore {
    fn get(&self, _key: &[u8]) -> Option<Bytes> {
        None
    }
    fn scan(&self, _prefix: &[u8]) -> Vec<Row> {
        vec![]
    }
}

impl WriteStore for FakeStore {
    fn write(&self, _rows_vec: Vec<Vec<Row>>) {}
    fn flush(&self) {}
}
