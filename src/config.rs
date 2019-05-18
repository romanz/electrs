use bitcoin::network::constants::Network;
use clap::{App, Arg};
use dirs::home_dir;
use num_cpus;
use std::fs;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use stderrlog;

use crate::daemon::CookieGetter;
use crate::errors::*;

const DEFAULT_SERVER_ADDRESS: &str = "127.0.0.1"; // by default, serve on IPv4 localhost

#[derive(Debug)]
pub struct Config {
    // See below for the documentation of each field:
    pub log: stderrlog::StdErrLog,
    pub network_type: Network,
    pub db_path: PathBuf,
    pub daemon_dir: PathBuf,
    pub daemon_rpc_addr: SocketAddr,
    pub cookie: Option<String>,
    pub electrum_rpc_addr: SocketAddr,
    pub monitoring_addr: SocketAddr,
    pub jsonrpc_import: bool,
    pub index_batch_size: usize,
    pub bulk_index_threads: usize,
    pub tx_cache_size: usize,
    pub txid_limit: usize,
    pub server_banner: String,
}

fn str_to_socketaddr(address: &str, what: &str) -> SocketAddr {
    address
        .to_socket_addrs()
        .unwrap_or_else(|e| panic!("unable to resolve {} address: {}", what, e))
        .next()
        .unwrap_or_else(|| panic!("no address found for {}", address))
}

impl Config {
    pub fn from_args() -> Config {
        let default_banner = format!(
            "Welcome to electrs {} (Electrum Rust Server)!",
            env!("CARGO_PKG_VERSION")
        );
        let m = App::new("Electrum Rust Server")
            .version(crate_version!())
            .arg(
                Arg::with_name("verbosity")
                    .short("v")
                    .multiple(true)
                    .help("Increase logging verbosity"),
            )
            .arg(
                Arg::with_name("timestamp")
                    .long("timestamp")
                    .help("Prepend log lines with a timestamp"),
            )
            .arg(
                Arg::with_name("db_dir")
                    .long("db-dir")
                    .help("Directory to store index database (default: ./db/)")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("daemon_dir")
                    .long("daemon-dir")
                    .help("Data directory of Bitcoind (default: ~/.bitcoin/)")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("cookie")
                    .long("cookie")
                    .help("JSONRPC authentication cookie ('USER:PASSWORD', default: read from ~/.bitcoin/.cookie)")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("network")
                    .long("network")
                    .help("Select Bitcoin network type ('mainnet', 'testnet' or 'regtest')")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("electrum_rpc_addr")
                    .long("electrum-rpc-addr")
                    .help("Electrum server JSONRPC 'addr:port' to listen on (default: '127.0.0.1:50001' for mainnet, '127.0.0.1:60001' for testnet and '127.0.0.1:60401' for regtest)")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("daemon_rpc_addr")
                    .long("daemon-rpc-addr")
                    .help("Bitcoin daemon JSONRPC 'addr:port' to connect (default: 127.0.0.1:8332 for mainnet, 127.0.0.1:18332 for testnet and 127.0.0.1:18443 for regtest)")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("monitoring_addr")
                    .long("monitoring-addr")
                    .help("Prometheus monitoring 'addr:port' to listen on (default: 127.0.0.1:4224 for mainnet, 127.0.0.1:14224 for testnet and 127.0.0.1:24224 for regtest)")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("jsonrpc_import")
                    .long("jsonrpc-import")
                    .help("Use JSONRPC instead of directly importing blk*.dat files. Useful for remote full node or low memory system"),
            )
            .arg(
                Arg::with_name("index_batch_size")
                    .long("index-batch-size")
                    .help("Number of blocks to get in one JSONRPC request from bitcoind")
                    .default_value("100"),
            )
            .arg(
                Arg::with_name("bulk_index_threads")
                    .long("bulk-index-threads")
                    .help("Number of threads used for bulk indexing (default: use the # of CPUs)")
                    .default_value("0")
            )
            .arg(
                Arg::with_name("tx_cache_size")
                    .long("tx-cache-size")
                    .help("Number of transactions to keep in for query LRU cache")
                    .default_value("10000")  // should be enough for a small wallet.
            )
            .arg(
                Arg::with_name("txid_limit")
                    .long("txid-limit")
                    .help("Number of transactions to lookup before returning an error, to prevent \"too popular\" addresses from causing the RPC server to get stuck (0 - disable the limit)")
                    .default_value("100")  // should take a few seconds on a HDD
            )
            .arg(
                Arg::with_name("server_banner")
                    .long("server-banner")
                    .help("The banner to be shown in the Electrum console")
                    .default_value(&default_banner)
            )
            .get_matches();

        let network_name = m.value_of("network").unwrap_or("mainnet");
        let network_type = match network_name {
            "mainnet" => Network::Bitcoin,
            "testnet" => Network::Testnet,
            "regtest" => Network::Regtest,
            _ => panic!("unsupported Bitcoin network: {:?}", network_name),
        };
        let db_dir = Path::new(m.value_of("db_dir").unwrap_or("./db"));
        let db_path = db_dir.join(network_name);

        let default_daemon_port = match network_type {
            Network::Bitcoin => 8332,
            Network::Testnet => 18332,
            Network::Regtest => 18443,
        };
        let default_electrum_port = match network_type {
            Network::Bitcoin => 50001,
            Network::Testnet => 60001,
            Network::Regtest => 60401,
        };
        let default_monitoring_port = match network_type {
            Network::Bitcoin => 4224,
            Network::Testnet => 14224,
            Network::Regtest => 24224,
        };

        let daemon_rpc_addr: SocketAddr = str_to_socketaddr(
            m.value_of("daemon_rpc_addr").unwrap_or(&format!(
                "{}:{}",
                DEFAULT_SERVER_ADDRESS, default_daemon_port
            )),
            "Bitcoin RPC",
        );
        let electrum_rpc_addr: SocketAddr = str_to_socketaddr(
            m.value_of("electrum_rpc_addr").unwrap_or(&format!(
                "{}:{}",
                DEFAULT_SERVER_ADDRESS, default_electrum_port
            )),
            "Electrum RPC",
        );
        let monitoring_addr: SocketAddr = str_to_socketaddr(
            m.value_of("monitoring_addr").unwrap_or(&format!(
                "{}:{}",
                DEFAULT_SERVER_ADDRESS, default_monitoring_port
            )),
            "Prometheus monitoring",
        );

        let mut daemon_dir = m
            .value_of("daemon_dir")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let mut default_dir = home_dir().expect("no homedir");
                default_dir.push(".bitcoin");
                default_dir
            });
        match network_type {
            Network::Bitcoin => (),
            Network::Testnet => daemon_dir.push("testnet3"),
            Network::Regtest => daemon_dir.push("regtest"),
        }
        let cookie = m.value_of("cookie").map(std::borrow::ToOwned::to_owned);

        let mut log = stderrlog::new();
        log.verbosity(m.occurrences_of("verbosity") as usize);
        log.timestamp(if m.is_present("timestamp") {
            stderrlog::Timestamp::Millisecond
        } else {
            stderrlog::Timestamp::Off
        });
        log.init().expect("logging initialization failed");
        let mut bulk_index_threads = value_t_or_exit!(m, "bulk_index_threads", usize);
        if bulk_index_threads == 0 {
            bulk_index_threads = num_cpus::get();
        }
        let config = Config {
            log,
            network_type,
            db_path,
            daemon_dir,
            daemon_rpc_addr,
            cookie,
            electrum_rpc_addr,
            monitoring_addr,
            jsonrpc_import: m.is_present("jsonrpc_import"),
            index_batch_size: value_t_or_exit!(m, "index_batch_size", usize),
            bulk_index_threads,
            tx_cache_size: value_t_or_exit!(m, "tx_cache_size", usize),
            txid_limit: value_t_or_exit!(m, "txid_limit", usize),
            server_banner: value_t_or_exit!(m, "server_banner", String),
        };
        eprintln!("{:?}", config);
        config
    }

    pub fn cookie_getter(&self) -> Arc<CookieGetter> {
        if let Some(ref value) = self.cookie {
            Arc::new(StaticCookie {
                value: value.as_bytes().to_vec(),
            })
        } else {
            Arc::new(CookieFile {
                daemon_dir: self.daemon_dir.clone(),
            })
        }
    }
}

struct StaticCookie {
    value: Vec<u8>,
}

impl CookieGetter for StaticCookie {
    fn get(&self) -> Result<Vec<u8>> {
        Ok(self.value.clone())
    }
}

struct CookieFile {
    daemon_dir: PathBuf,
}

impl CookieGetter for CookieFile {
    fn get(&self) -> Result<Vec<u8>> {
        let path = self.daemon_dir.join(".cookie");
        let contents = fs::read(&path).chain_err(|| {
            ErrorKind::Connection(format!("failed to read cookie from {:?}", path))
        })?;
        Ok(contents)
    }
}
