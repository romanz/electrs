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
mod util;
mod waiter;

use store::{Store, StoreOptions};

type Bytes = Vec<u8>;

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
        if store.has_block(&waiter.wait()) {
            continue;
        }
        index::update(&mut store, &daemon);
    }
}
