extern crate simplelog;

extern crate argparse;
extern crate crossbeam;
extern crate indexrs;

use argparse::{ArgumentParser, StoreFalse, StoreTrue};
use std::fs::OpenOptions;
use indexrs::{daemon, index, query, rpc, store, waiter};

struct Config {
    log_file: String,
    enable_indexing: bool,
    testnet: bool,
}

impl Config {
    pub fn from_args() -> Config {
        let mut config = Config {
            log_file: "indexrs.log".to_string(),
            enable_indexing: true,
            testnet: false,
        };
        {
            let mut parser = ArgumentParser::new();
            parser.set_description("Bitcoin indexing server.");
            parser.refer(&mut config.enable_indexing).add_option(
                &["--disable-indexing"],
                StoreFalse,
                "Disable indexing server (allow queries on existing DB)",
            );
            parser.refer(&mut config.testnet).add_option(
                &["--testnet"],
                StoreTrue,
                "Connect to a testnet bitcoind instance",
            );
            parser.parse_args_or_exit();
        }
        config
    }

    pub fn rpc_addr(&self) -> &'static str {
        // for serving Electrum clients
        match self.testnet {
            false => "localhost:50001",
            true => "localhost:60001",
        }
    }

    pub fn db_path(&self) -> &'static str {
        match self.testnet {
            false => "./db/mainnet",
            true => "./db/testnet",
        }
    }

    pub fn daemon_url(&self) -> &'static str {
        match self.testnet {
            false => "http://localhost:8332",
            true => "http://localhost:18332",
        }
    }

    pub fn zmq_endpoint(&self) -> &'static str {
        "tcp://localhost:28332"
    }
}

fn run_server(config: &Config) {
    let index = index::Index::new();
    let waiter = waiter::Waiter::new(config.zmq_endpoint());
    let daemon = daemon::Daemon::new(config.daemon_url());
    {
        let store = store::Store::open(
            config.db_path(),
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

    let store = store::Store::open(config.db_path(), store::StoreOptions { auto_compact: true });
    let query = query::Query::new(&store, &daemon, &index);

    crossbeam::scope(|scope| {
        let chan = rpc::Channel::new();
        let tx = chan.sender();
        scope.spawn(|| rpc::serve(config.rpc_addr(), &query, chan));
        loop {
            let blockhash = waiter.wait();
            if config.enable_indexing {
                index.update(&store, &daemon);
            }
            tx.send(rpc::Message::Block(blockhash)).unwrap();
        }
    });
}

fn setup_logging(config: &Config) {
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
                .open(&config.log_file)
                .unwrap(),
        ),
    ]).unwrap();
}

fn main() {
    let config = Config::from_args();
    setup_logging(&config);
    run_server(&config)
}
