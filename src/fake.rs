use crate::store::{ReadStore, Row, WriteStore};
use crate::util::Bytes;

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
    fn write<I: IntoIterator<Item = Row>>(&self, _rows: I) {}
    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_fakestore() {
        use crate::fake;
        use crate::store::{ReadStore, Row, WriteStore};

        let store = fake::FakeStore {};
        store.write(vec![Row {
            key: b"k".to_vec(),
            value: b"v".to_vec(),
        }]);
        store.flush();
        // nothing was actually written
        assert!(store.get(b"").is_none());
        assert!(store.scan(b"").is_empty());
    }
}
