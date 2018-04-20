extern crate simplelog;

extern crate crossbeam;
extern crate indexrs;
extern crate zmq;

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
        let store = store::Store::open(
            DB_PATH,
            store::StoreOptions {
                auto_compact: false,
            },
        );
        index::update(&store, &daemon);
        store.compact_if_needed();
    }

    let store = store::Store::open(DB_PATH, store::StoreOptions { auto_compact: true });
    {
        crossbeam::scope(|scope| {
            scope.spawn(|| {
                let q = index::Query::new(&store, &daemon);

                let ctx = zmq::Context::new();
                let sock = ctx.socket(zmq::SocketType::REP).unwrap();
                sock.bind("tcp://127.0.0.1:19740").unwrap();

                loop {
                    let script_hash = sock.recv_bytes(0).unwrap();
                    let b = q.balance(&script_hash);
                    let reply = format!("{}", b);
                    sock.send(&reply.into_bytes(), 0).unwrap();
                }
            });
            loop {
                if store.read_header(&waiter.wait()).is_none() {
                    index::update(&store, &daemon);
                }
            }
        });
    }
}

fn main() {
    setup_logging();
    run_server()
}
