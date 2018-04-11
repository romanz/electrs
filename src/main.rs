extern crate bitcoin;
extern crate byteorder;
extern crate crypto;
extern crate itertools;
extern crate pbr;
extern crate reqwest;
extern crate rocksdb;
extern crate simple_logger;
extern crate time;
extern crate zmq;

#[macro_use]
extern crate log;

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

fn main() {
    simple_logger::init_with_level(log::Level::Info).unwrap();
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
