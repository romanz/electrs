extern crate electrs;

extern crate error_chain;

#[macro_use]
extern crate log;

use error_chain::ChainedError;

use electrs::{config::Config,
              daemon::Daemon,
              errors::*,
              index::Index,
              store::{ReadStore, Row, WriteStore},
              util::Bytes};

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
}

fn run() -> Result<()> {
    let config = Config::from_args();
    let daemon = Daemon::new(config.network_type)?;
    debug!("{:?}", daemon.getblockchaininfo()?);
    let fake_store = FakeStore {};
    let index = Index::load(&fake_store);
    let tip = index.update(&fake_store, &daemon)?;
    info!("downloaded and indexed till {}", tip);
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        error!("{}", e.display_chain());
    }
}
