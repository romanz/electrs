extern crate simplelog;

extern crate argparse;
extern crate crossbeam;
extern crate indexrs;

use argparse::{ArgumentParser, StoreFalse};
use std::fs::OpenOptions;
use indexrs::{daemon, index, query, rpc, store, waiter};

fn setup_logging() {
    use simplelog::*;
    let mut cfg = Config::default();
    cfg.time_format = Some("%F %H:%M:%S%.3f");
    CombinedLogger::init(vec![
        TermLogger::new(LevelFilter::Info, cfg.clone()).unwrap(),
        WriteLogger::new(
            LevelFilter::Debug,
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

struct Config {
    enable_indexing: bool,
}

fn run_server(config: Config) {
    let index = index::Index::new();
    let waiter = waiter::Waiter::new("tcp://localhost:28332");
    let daemon = daemon::Daemon::new("http://localhost:8332");
    {
        let store = store::Store::open(
            DB_PATH,
            store::StoreOptions {
                // compact manually after the first run has finished successfully
                auto_compact: false,
            },
        );
        if config.enable_indexing {
            index.update(&store, &daemon);
            store.compact_if_needed();
        }
    }

    let store = store::Store::open(DB_PATH, store::StoreOptions { auto_compact: true });
    let query = query::Query::new(&store, &daemon, &index);

    crossbeam::scope(|scope| {
        let chan = rpc::Channel::new();
        let tx = chan.sender();
        scope.spawn(|| rpc::serve("localhost:50001", &query, chan));
        loop {
            let blockhash = waiter.wait();
            tx.send(rpc::Message::Block(blockhash)).unwrap();
            if config.enable_indexing {
                index.update(&store, &daemon);
            }
        }
    });
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
