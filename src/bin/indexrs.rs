extern crate argparse;
extern crate bitcoin;
extern crate crossbeam;
extern crate indexrs;
extern crate simplelog;

#[macro_use]
extern crate log;

use argparse::{ArgumentParser, StoreTrue};
use indexrs::daemon::Network;
use indexrs::{daemon, index, query, rpc, store};
use std::fs::OpenOptions;
use std::thread;
use std::time::Duration;

#[derive(Debug)]
struct Config {
    log_file: String,
    testnet: bool,
}

impl Config {
    pub fn from_args() -> Config {
        let mut config = Config {
            log_file: "indexrs.log".to_string(),
            testnet: false,
        };
        {
            let mut parser = ArgumentParser::new();
            parser.set_description("Bitcoin indexing server.");
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
        match self.network_type() {
            Network::Mainnet => "localhost:50001",
            Network::Testnet => "localhost:60001",
        }
    }

    pub fn db_path(&self) -> &'static str {
        match self.network_type() {
            Network::Mainnet => "./db/mainnet",
            Network::Testnet => "./db/testnet",
        }
    }

    pub fn network_type(&self) -> Network {
        // bitcoind JSONRPC endpoint
        match self.testnet {
            false => Network::Mainnet,
            true => Network::Testnet,
        }
    }
}

fn run_server(config: &Config) {
    // TODO: handle SIGINT gracefully (https://www.reddit.com/r/rust/comments/6swidb/how_to_properly_catch_sigint_in_a_threaded_program/)
    let index = index::Index::new();
    let daemon = daemon::Daemon::new(config.network_type());

    let store = store::DBStore::open(
        config.db_path(),
        store::StoreOptions {
            // compact manually after the first run has finished successfully
            auto_compact: false,
        },
    );
    let mut tip = index.update(&store, &daemon);
    store.compact_if_needed();
    drop(store); // to be re-opened soon

    let store = store::DBStore::open(config.db_path(), store::StoreOptions { auto_compact: true });
    let query = query::Query::new(&store, &daemon, &index);

    crossbeam::scope(|scope| {
        let poll_delay = Duration::from_secs(5);
        scope.spawn(|| rpc::serve(config.rpc_addr(), &query));
        loop {
            thread::sleep(poll_delay);
            query.update_mempool().unwrap();
            if tip != daemon.getbestblockhash().unwrap() {
                tip = index.update(&store, &daemon);
            }
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
    info!("config: {:?}", config);
}

fn main() {
    let config = Config::from_args();
    setup_logging(&config);
    run_server(&config)
}
