extern crate electrs;
extern crate error_chain;

use electrs::{config::Config,
              daemon::Daemon,
              errors::*,
              index::Index,
              store::{ReadStore, Row, WriteStore},
              util::Bytes};
use error_chain::ChainedError;

struct FakeStore;

impl ReadStore for FakeStore {
    fn get(&self, _key: &[u8]) -> Option<Bytes> {
        None
    }
    fn scan(&self, _prefix: &[u8]) -> Vec<Row> {
        vec![]
    }
}

impl WriteStore for FakeStore {
    fn write(&self, _rows: Vec<Row>) {}
    fn flush(&self) {}
}

fn run() -> Result<()> {
    let config = Config::from_args();
    let daemon = Daemon::new(config.network_type)?;
    let fake_store = FakeStore {};
    let index = Index::load(&fake_store);
    index.update(&fake_store, &daemon)?;
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{}", e.display_chain());
    }
}
