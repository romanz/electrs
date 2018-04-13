extern crate simplelog;

extern crate indexrs;

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
            std::fs::File::create("indexrs.log").unwrap(),
        ),
    ]).unwrap();
}

fn run_server() {
    let waiter = waiter::Waiter::new("tcp://localhost:28332");
    let daemon = daemon::Daemon::new("http://localhost:8332");
    {
        let mut store = store::Store::open(
            "db/mainnet",
            store::StoreOptions {
                auto_compact: false,
            },
        );
        index::update(&mut store, &daemon);
        store.compact_if_needed();
    }

    let mut store = store::Store::open("db/mainnet", store::StoreOptions { auto_compact: true });
    loop {
        if store.read_header(&waiter.wait()).is_some() {
            continue;
        }
        index::update(&mut store, &daemon);
    }
}

fn main() {
    setup_logging();
    run_server()
}
