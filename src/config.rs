use argparse::{ArgumentParser, Store, StoreTrue};
use daemon::Network;
use simplelog;
use std::fs::OpenOptions;
use std::net::SocketAddr;

#[derive(Debug)]
pub struct Config {
    pub log_file: String,
    pub log_level: simplelog::LevelFilter,
    pub restart: bool,
    pub network_type: Network, // bitcoind JSONRPC endpoint
    pub db_path: &'static str, // RocksDB directory path
    pub rpc_addr: SocketAddr,  // for serving Electrum clients
}

impl Config {
    pub fn from_args() -> Config {
        let mut testnet = false;
        let mut verbose = false;
        let mut restart = false;
        let mut log_file = "".to_string();
        {
            let mut parser = ArgumentParser::new();
            parser.set_description("Bitcoin indexing server.");
            parser.refer(&mut testnet).add_option(
                &["--testnet"],
                StoreTrue,
                "Connect to a testnet bitcoind instance",
            );
            parser.refer(&mut verbose).add_option(
                &["-v", "--verbose"],
                StoreTrue,
                "More verbose logging to stderr",
            );
            parser.refer(&mut restart).add_option(
                &["--restart"],
                StoreTrue,
                "Restart the server in case of a recoverable error",
            );
            parser.refer(&mut log_file).add_option(
                &["-l", "--log-file"],
                Store,
                "Write the log into specified file",
            );
            parser.parse_args_or_exit();
        }
        let network_type = match testnet {
            false => Network::Mainnet,
            true => Network::Testnet,
        };
        let config = Config {
            log_file,
            log_level: if verbose {
                simplelog::LevelFilter::Debug
            } else {
                simplelog::LevelFilter::Info
            },
            restart,
            network_type,
            db_path: match network_type {
                Network::Mainnet => "./db/mainnet",
                Network::Testnet => "./db/testnet",
            },
            rpc_addr: match network_type {
                Network::Mainnet => "127.0.0.1:50001",
                Network::Testnet => "127.0.0.1:60001",
            }.parse()
                .unwrap(),
        };
        setup_logging(&config);
        config
    }
}

fn setup_logging(config: &Config) {
    let mut cfg = simplelog::Config::default();
    cfg.time_format = Some("%F %H:%M:%S%.3f");
    let mut loggers = Vec::<Box<simplelog::SharedLogger>>::new();
    loggers.push(simplelog::TermLogger::new(config.log_level, cfg.clone()).unwrap());
    if !config.log_file.is_empty() {
        loggers.push(simplelog::WriteLogger::new(
            simplelog::LevelFilter::Debug,
            cfg.clone(),
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&config.log_file)
                .unwrap(),
        ));
    }
    simplelog::CombinedLogger::init(loggers).unwrap();
    info!("config: {:?}", config);
}
