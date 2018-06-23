extern crate bitcoin;
extern crate electrs;

#[macro_use]
extern crate log;
#[macro_use]
extern crate error_chain;

use bitcoin::network::serialize::BitcoinHash;
use bitcoin::util::hash::Sha256dHash;
use std::collections::HashMap;
use std::iter::FromIterator;

use electrs::{config::Config,
              daemon::Daemon,
              errors::*,
              index,
              metrics::Metrics,
              parse::Parser,
              signal::Waiter,
              store::{ReadStore, Row, WriteStore},
              util::{Bytes, HeaderEntry, HeaderList}};

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

fn run(config: Config) -> Result<()> {
    let signal = Waiter::new();
    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();

    let daemon = Daemon::new(config.network_type, &metrics)?;
    let fake_store = FakeStore {};

    let tip = daemon.getbestblockhash()?;
    let new_headers: Vec<HeaderEntry> = {
        let indexed_headers = HeaderList::empty();
        indexed_headers.order(daemon.get_new_headers(&indexed_headers, &tip)?)
    };
    new_headers.last().map(|tip| {
        info!("{:?} ({} left to index)", tip, new_headers.len());
    });
    let height_map = HashMap::<Sha256dHash, usize>::from_iter(
        new_headers.iter().map(|h| (*h.hash(), h.height())),
    );

    let chan = Parser::new(&daemon, &metrics)?.start();
    for blocks in chan.iter() {
        if let Some(sig) = signal.poll() {
            bail!("indexing interrupted by SIG{:?}", sig);
        }
        let blocks = blocks?;
        for block in &blocks {
            let blockhash = block.bitcoin_hash();
            if let Some(height) = height_map.get(&blockhash) {
                let rows = index::index_block(block, *height);
                fake_store.write(rows);
            } else {
                warn!("unknown block {}", blockhash);
            }
        }
        trace!("indexed {} blocks", blocks.len());
    }
    debug!("done");
    Ok(())
}

fn main() {
    if let Err(e) = run(Config::from_args()) {
        eprintln!("{}", e.display_chain());
    }
}
