extern crate bincode;
extern crate bitcoin;
extern crate crypto;
extern crate itertools;
extern crate pbr;
extern crate reqwest;
extern crate rocksdb;
extern crate serde;
extern crate simplelog;
extern crate time;
extern crate zmq;

#[macro_use]
extern crate arrayref;
#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_derive;

mod daemon;
mod index;
mod store;
mod timer;
mod waiter;

use bitcoin::blockdata::block::BlockHeader;
use bitcoin::util::hash::Sha256dHash;
use std::collections::HashMap;
use store::{Store, StoreOptions};

type Bytes = Vec<u8>;
type HeaderMap = HashMap<Sha256dHash, BlockHeader>;

fn setup_logging() {
    use simplelog::*;
    let mut cfg = Config::default();
    cfg.time_format = Some("%F %H:%M:%S%.3f");
    CombinedLogger::init(vec![
        TermLogger::new(LevelFilter::Info, cfg.clone()).unwrap(),
        WriteLogger::new(
            LevelFilter::Info,
            cfg.clone(),
            std::fs::File::create("indexrs.log").unwrap(),
        ),
    ]).unwrap();
}

fn main() {
    setup_logging();
    let waiter = waiter::Waiter::new("tcp://localhost:28332");
    let daemon = daemon::Daemon::new("http://localhost:8332");
    {
        let mut store = Store::open(
            "db/mainnet",
            StoreOptions {
                auto_compact: false,
            },
        );
        index::update(&mut store, &daemon);
        store.compact_if_needed();
    }

    let mut store = Store::open("db/mainnet", StoreOptions { auto_compact: true });
    loop {
        if store.read_header(&waiter.wait()).is_some() {
            continue;
        }
        index::update(&mut store, &daemon);
    }
}
