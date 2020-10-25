use bitcoin::Network;

use clap::{App, Arg};

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug)]
pub struct Config {
    pub electrum_rpc_addr: SocketAddr,
    pub daemon_rpc_addr: SocketAddr,
    pub monitoring_addr: SocketAddr,
    pub db_path: PathBuf,
    pub daemon_dir: PathBuf,
    pub wait_duration: Duration,
    pub args: Vec<String>,
}

impl Config {
    pub fn from_args() -> Self {
        let matches = App::new("Electrum Server in Rust")
            .arg(
                Arg::with_name("network")
                    .long("network")
                    .help("mainnet/testnet/regtest")
                    .required(true)
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("db-dir")
                    .long("db-dir")
                    .help("RocksDB directory")
                    .default_value("./db")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("daemon-dir")
                    .long("daemon-dir")
                    .help("bitcoind directory")
                    .takes_value(true),
            )
            .arg(Arg::with_name("args").takes_value(true).multiple(true))
            .get_matches();

        let network_str = matches.value_of("network").unwrap();
        let network = match network_str {
            "mainnet" => Network::Bitcoin,
            "testnet" => Network::Testnet,
            "regtest" => Network::Regtest,
            _ => panic!("unknown network"),
        };

        let electrum_port = match network {
            Network::Bitcoin => 50001,
            Network::Testnet => 60001,
            Network::Regtest => 60401,
        };
        let electrum_rpc_addr: SocketAddr = ([127, 0, 0, 1], electrum_port).into();

        let daemon_port = match network {
            Network::Bitcoin => 8332,
            Network::Testnet => 18332,
            Network::Regtest => 18443,
        };
        let daemon_rpc_addr: SocketAddr = ([127, 0, 0, 1], daemon_port).into();

        let monitoring_port = match network {
            Network::Bitcoin => 4224,
            Network::Testnet => 14224,
            Network::Regtest => 24224,
        };
        let monitoring_addr: SocketAddr = ([127, 0, 0, 1], monitoring_port).into();

        let daemon_dir: PathBuf = matches.value_of("daemon-dir").unwrap().into();
        let daemon_dir = match network {
            Network::Bitcoin => daemon_dir,
            Network::Testnet => daemon_dir.join("testnet3"),
            Network::Regtest => daemon_dir.join("regtest"),
        };

        let args = matches
            .values_of("args")
            .unwrap()
            .map(String::from)
            .collect();

        let mut db_path: PathBuf = matches.value_of("db-dir").unwrap().into();
        db_path.push(network_str);

        env_logger::Builder::from_default_env()
            .default_format()
            .format_timestamp_millis()
            .init();

        Self {
            electrum_rpc_addr,
            daemon_rpc_addr,
            monitoring_addr,
            db_path,
            daemon_dir,
            wait_duration: Duration::from_secs(600),
            args,
        }
    }
}
