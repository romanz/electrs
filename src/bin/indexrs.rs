extern crate argparse;
extern crate bitcoin;
extern crate crossbeam;
extern crate indexrs;
extern crate simplelog;

#[macro_use]
extern crate log;

use argparse::{ArgumentParser, StoreTrue};
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

    pub fn daemon_addr(&self) -> &'static str {
        // bitcoind JSONRPC endpoint
        match self.testnet {
            false => "localhost:8332",
            true => "localhost:18332",
        }
    }
}

fn run_server(config: &Config) {
    // TODO: handle SIGINT gracefully (https://www.reddit.com/r/rust/comments/6swidb/how_to_properly_catch_sigint_in_a_threaded_program/)
    let index = index::Index::new();
    let daemon = daemon::Daemon::new(config.daemon_addr());

    let store = store::Store::open(
        config.db_path(),
        store::StoreOptions {
            // compact manually after the first run has finished successfully
            auto_compact: false,
        },
    );
    let mut tip = index.update(&store, &daemon);
    store.compact_if_needed();
    drop(store);

    let store = store::Store::open(config.db_path(), store::StoreOptions { auto_compact: true });
    let query = query::Query::new(&store, &daemon, &index);

    crossbeam::scope(|scope| {
        let poll_delay = Duration::from_secs(1);
        let chan = rpc::Channel::new();
        let tx = chan.sender();
        scope.spawn(|| rpc::serve(config.rpc_addr(), &query, chan));
        loop {
            thread::sleep(poll_delay);
            let current_tip = daemon
                .getbestblockhash()
                .expect("failed to get latest blockhash");

            if tip == current_tip {
                continue;
            }
            tip = index.update(&store, &daemon);
            if let Err(e) = tx.try_send(rpc::Message::Block(tip)) {
                debug!("failed to update RPC server {}: {:?}", tip, e);
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
