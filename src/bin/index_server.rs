extern crate simplelog;

extern crate indexrs;

use std::fs::OpenOptions;
use indexrs::{daemon, index, store, waiter};

fn setup_logging() {
    use simplelog::*;
    let mut cfg = Config::default();
    cfg.time_format = Some("%F %H:%M:%S%.3f");
    CombinedLogger::init(vec![
        TermLogger::new(LevelFilter::Info, cfg.clone()).unwrap(),
        WriteLogger::new(
            LevelFilter::Info,
            cfg.clone(),
            OpenOptions::new()
                .create(true)
                .append(true)
                .open("indexrs.log")
                .unwrap(),
        ),
    ]).unwrap();
}

const DB_PATH: &str = "db/mainnet";

fn run_server() {
    let waiter = waiter::Waiter::new("tcp://localhost:28332");
    let daemon = daemon::Daemon::new("http://localhost:8332");
    {
        let mut store = store::Store::open(
            DB_PATH,
            store::StoreOptions {
                auto_compact: false,
            },
        );
        index::update(&mut store, &daemon);
        store.compact_if_needed();
    }

    let mut store = store::Store::open(DB_PATH, store::StoreOptions { auto_compact: true });
    {
        let q = index::Query::new(&store, &daemon);
    }
    loop {
        if store.read_header(&waiter.wait()).is_none() {
            index::update(&mut store, &daemon);
        }
    }
}

// let sh = b"w\xb9\x12\xca\xdb\x8d\xb6\x13Q|\x04\x94\x189v\xd0\x88\xf5\xae\xfc\x8c\x91\x9b\x14ID[\xa8G\x9d_\xdd";  // spent
// let sh = b"\xa6\xe5\xd1;)\x06p\xe0\x8a\xfc\xdf\xd5\xe0\xc5R\xfd+\xc1'\xad\x91t\xd1q\xebM7\xa0[\xc1\x9d\xa2";  // unspent
// let b = index::query(&store, &daemon, sh);
// println!("balance: {}", b);

fn main() {
    setup_logging();
    run_server()
}
