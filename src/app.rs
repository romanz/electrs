use argparse::{ArgumentParser, StoreTrue};
use crossbeam;
use std::fs::OpenOptions;
use std::net::SocketAddr;
use std::thread;
use std::time::Duration;

use daemon::Network;
use {daemon, index, query, rpc, store};

#[derive(Debug)]
struct Config {
    log_file: String,
    network_type: Network, // bitcoind JSONRPC endpoint
    db_path: &'static str, // RocksDB directory path
    rpc_addr: SocketAddr,  // for serving Electrum clients
}

impl Config {
    pub fn from_args() -> Config {
        let mut testnet = false;
        {
            let mut parser = ArgumentParser::new();
            parser.set_description("Bitcoin indexing server.");
            parser.refer(&mut testnet).add_option(
                &["--testnet"],
                StoreTrue,
                "Connect to a testnet bitcoind instance",
            );
            parser.parse_args_or_exit();
        }
        let network_type = match testnet {
            false => Network::Mainnet,
            true => Network::Testnet,
        };
        Config {
            log_file: "indexrs.log".to_string(),
            network_type: network_type,
            db_path: match network_type {
                Network::Mainnet => "./db/mainnet",
                Network::Testnet => "./db/testnet",
            },
            rpc_addr: match network_type {
                Network::Mainnet => "127.0.0.1:50001",
                Network::Testnet => "127.0.0.1:60001",
            }.parse()
                .unwrap(),
        }
    }
}

fn run_server(config: &Config) {
    let index = index::Index::new();
    let daemon = daemon::Daemon::new(config.network_type);

    let store = store::DBStore::open(
        config.db_path,
        store::StoreOptions {
            // compact manually after the first run has finished successfully
            auto_compact: false,
        },
    );
    let mut tip = index.update(&store, &daemon);
    store.compact_if_needed();
    drop(store); // to be re-opened soon

    let store = store::DBStore::open(config.db_path, store::StoreOptions { auto_compact: true });
    let query = query::Query::new(&store, &daemon, &index);

    crossbeam::scope(|scope| {
        let poll_delay = Duration::from_secs(5);
        scope.spawn(|| rpc::serve(&config.rpc_addr, &query));
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

pub fn main() {
    let config = Config::from_args();
    setup_logging(&config);
    run_server(&config)
}
