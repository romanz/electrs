extern crate simplelog;

extern crate argparse;
extern crate crossbeam;
extern crate indexrs;
extern crate zmq;

use argparse::{ArgumentParser, StoreFalse};
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

const DB_PATH: &str = "./db/mainnet";
const RPC_ADDRESS: &str = "ipc:///tmp/indexrs.rpc";

struct Config {
    enable_indexing: bool,
}

fn handle_queries(store: &store::Store, daemon: &daemon::Daemon) {
    let query = index::Query::new(&store, &daemon);

    let ctx = zmq::Context::new();
    let sock = ctx.socket(zmq::SocketType::REP).unwrap();
    sock.bind(RPC_ADDRESS).unwrap();

    loop {
        let script_hash = sock.recv_bytes(0).unwrap();
        let balance = query.balance(&script_hash);
        let reply = format!("{}", balance);
        sock.send(&reply.into_bytes(), 0).unwrap();
    }
}

fn run_server(config: Config) {
    let waiter = waiter::Waiter::new("tcp://localhost:28332");
    let daemon = daemon::Daemon::new("http://localhost:8332");
    if config.enable_indexing {
        let store = store::Store::open(
            DB_PATH,
            store::StoreOptions {
                // compact manually after the first run has finished successfully
                auto_compact: false,
            },
        );
        index::update(&store, &daemon);
        store.compact_if_needed();
    }

    let store = store::Store::open(DB_PATH, store::StoreOptions { auto_compact: true });
    {
        crossbeam::scope(|scope| {
            scope.spawn(|| handle_queries(&store, &daemon));
            if config.enable_indexing {
                loop {
                    if store.read_header(&waiter.wait()).is_none() {
                        index::update(&store, &daemon);
                    }
                }
            }
        });
    }
}

fn main() {
    let mut config = Config {
        enable_indexing: true,
    };
    {
        let mut parser = ArgumentParser::new();
        parser.set_description("Bitcoin indexing server.");
        parser.refer(&mut config.enable_indexing).add_option(
            &["--disable-indexing"],
            StoreFalse,
            "Disable indexing server (allow queries on existing DB)",
        );
        parser.parse_args_or_exit();
    }

    setup_logging();
    run_server(config)
}
