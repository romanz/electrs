use clap::{App, Arg};
use dirs::home_dir;
use std::fs;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use stderrlog;

use crate::chain::Network;
use crate::daemon::CookieGetter;
use crate::errors::*;

#[cfg(feature = "liquid")]
use bitcoin::Network as BNetwork;

const ELECTRS_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone)]
pub struct Config {
    // See below for the documentation of each field:
    pub log: stderrlog::StdErrLog,
    pub network_type: Network,
    pub db_path: PathBuf,
    pub daemon_dir: PathBuf,
    pub blocks_dir: PathBuf,
    pub daemon_rpc_addr: SocketAddr,
    pub daemon_parallelism: usize,
    pub cookie: Option<String>,
    pub electrum_rpc_addr: SocketAddr,
    pub http_addr: SocketAddr,
    pub http_socket_file: Option<PathBuf>,
    pub monitoring_addr: SocketAddr,
    pub jsonrpc_import: bool,
    pub light_mode: bool,
    pub address_search: bool,
    pub index_unspendables: bool,
    pub cors: Option<String>,
    pub precache_scripts: Option<String>,
    pub utxos_limit: usize,
    pub electrum_txs_limit: usize,
    pub electrum_banner: String,
    pub electrum_rpc_logging: Option<RpcLogging>,
    pub zmq_addr: Option<SocketAddr>,

    /// Enable compaction during initial sync
    ///
    /// By default compaction is off until initial sync is finished for performance reasons,
    /// however, this requires much more disk space.
    pub initial_sync_compaction: bool,

    #[cfg(feature = "liquid")]
    pub parent_network: BNetwork,
    #[cfg(feature = "liquid")]
    pub asset_db_path: Option<PathBuf>,

    #[cfg(feature = "electrum-discovery")]
    pub electrum_public_hosts: Option<crate::electrum::ServerHosts>,
    #[cfg(feature = "electrum-discovery")]
    pub electrum_announce: bool,
    #[cfg(feature = "electrum-discovery")]
    pub tor_proxy: Option<std::net::SocketAddr>,
}

fn str_to_socketaddr(address: &str, what: &str) -> SocketAddr {
    address
        .to_socket_addrs()
        .unwrap_or_else(|_| panic!("unable to resolve {} address", what))
        .collect::<Vec<_>>()
        .pop()
        .unwrap()
}

impl Config {
    pub fn from_args() -> Config {
        let network_help = format!("Select network type ({})", Network::names().join(", "));
        let rpc_logging_help = format!(
            "Select RPC logging option ({})",
            RpcLogging::options().join(", ")
        );

        let args = App::new("Electrum Rust Server")
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
                Arg::with_name("blocks_dir")
                    .long("blocks-dir")
                    .help("Analogous to bitcoind's -blocksdir option, this specifies the directory containing the raw blocks files (blk*.dat) (default: ~/.bitcoin/blocks/)")
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
                    .help(&network_help)
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("electrum_rpc_addr")
                    .long("electrum-rpc-addr")
                    .help("Electrum server JSONRPC 'addr:port' to listen on (default: '127.0.0.1:50001' for mainnet, '127.0.0.1:60001' for testnet3, '127.0.0.1:40001' for testnet4 and '127.0.0.1:60401' for regtest)")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("http_addr")
                    .long("http-addr")
                    .help("HTTP server 'addr:port' to listen on (default: '127.0.0.1:3000' for mainnet, '127.0.0.1:3001' for testnet3 and '127.0.0.1:3004' for testnet4 and '127.0.0.1:3002' for regtest)")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("daemon_rpc_addr")
                    .long("daemon-rpc-addr")
                    .help("Bitcoin daemon JSONRPC 'addr:port' to connect (default: 127.0.0.1:8332 for mainnet, 127.0.0.1:18332 for testnet3 and 127.0.0.1:48332 for testnet4 and 127.0.0.1:18443 for regtest)")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("daemon_parallelism")
                    .long("daemon-parallelism")
                    .help("Number of JSONRPC requests to send in parallel")
                    .default_value("4")
            )
            .arg(
                Arg::with_name("monitoring_addr")
                    .long("monitoring-addr")
                    .help("Prometheus monitoring 'addr:port' to listen on (default: 127.0.0.1:4224 for mainnet, 127.0.0.1:14224 for testnet3 and 127.0.0.1:44224 for testnet4 and 127.0.0.1:24224 for regtest)")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("jsonrpc_import")
                    .long("jsonrpc-import")
                    .help("Use JSONRPC instead of directly importing blk*.dat files. Useful for remote full node or low memory system"),
            )
            .arg(
                Arg::with_name("light_mode")
                    .long("lightmode")
                    .help("Enable light mode for reduced storage")
            )
            .arg(
                Arg::with_name("address_search")
                    .long("address-search")
                    .help("Enable prefix address search")
            )
            .arg(
                Arg::with_name("index_unspendables")
                    .long("index-unspendables")
                    .help("Enable indexing of provably unspendable outputs")
            )
            .arg(
                Arg::with_name("cors")
                    .long("cors")
                    .help("Origins allowed to make cross-site requests")
                    .takes_value(true)
            )
            .arg(
                Arg::with_name("precache_scripts")
                    .long("precache-scripts")
                    .help("Path to file with list of scripts to pre-cache")
                    .takes_value(true)
            )
            .arg(
                Arg::with_name("utxos_limit")
                    .long("utxos-limit")
                    .help("Maximum number of utxos to process per address. Lookups for addresses with more utxos will fail. Applies to the Electrum and HTTP APIs.")
                    .default_value("500")
            )
            .arg(
                Arg::with_name("electrum_txs_limit")
                    .long("electrum-txs-limit")
                    .help("Maximum number of transactions returned by Electrum history queries. Lookups with more results will fail.")
                    .default_value("500")
            ).arg(
                Arg::with_name("electrum_banner")
                    .long("electrum-banner")
                    .help("Welcome banner for the Electrum server, shown in the console to clients.")
                    .takes_value(true)
            ).arg(
                Arg::with_name("electrum_rpc_logging")
                    .long("electrum-rpc-logging")
                    .help(&rpc_logging_help)
                    .takes_value(true),
            ).arg(
                Arg::with_name("initial_sync_compaction")
                    .long("initial-sync-compaction")
                    .help("Perform compaction during initial sync (slower but less disk space required)")
            ).arg(
                Arg::with_name("zmq_addr")
                    .long("zmq-addr")
                    .help("Optional zmq socket address of the bitcoind daemon")
                    .takes_value(true),
            );

        #[cfg(unix)]
        let args = args.arg(
                Arg::with_name("http_socket_file")
                    .long("http-socket-file")
                    .help("HTTP server 'unix socket file' to listen on (default disabled, enabling this disables the http server)")
                    .takes_value(true),
            );

        #[cfg(feature = "liquid")]
        let args = args
            .arg(
                Arg::with_name("parent_network")
                    .long("parent-network")
                    .help("Select parent network type (mainnet, testnet, regtest)")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("asset_db_path")
                    .long("asset-db-path")
                    .help("Directory for liquid/elements asset db")
                    .takes_value(true),
            );

        #[cfg(feature = "electrum-discovery")]
        let args = args.arg(
                Arg::with_name("electrum_public_hosts")
                    .long("electrum-public-hosts")
                    .help("A dictionary of hosts where the Electrum server can be reached at. Required to enable server discovery. See https://electrumx.readthedocs.io/en/latest/protocol-methods.html#server-features")
                    .takes_value(true)
            ).arg(
                Arg::with_name("electrum_announce")
                    .long("electrum-announce")
                    .help("Announce the Electrum server to other servers")
            ).arg(
            Arg::with_name("tor_proxy")
                .long("tor-proxy")
                .help("ip:addr of socks proxy for accessing onion hosts")
                .takes_value(true),
        );

        let m = args.get_matches();

        let network_name = m.value_of("network").unwrap_or("mainnet");
        let network_type = Network::from(network_name);
        let db_dir = Path::new(m.value_of("db_dir").unwrap_or("./db"));
        let db_path = db_dir.join(network_name);

        #[cfg(feature = "liquid")]
        let parent_network = m
            .value_of("parent_network")
            .map(|s| s.parse().expect("invalid parent network"))
            .unwrap_or_else(|| match network_type {
                Network::Liquid => BNetwork::Bitcoin,
                // XXX liquid testnet/regtest don't have a parent chain
                Network::LiquidTestnet | Network::LiquidRegtest => BNetwork::Regtest,
            });

        #[cfg(feature = "liquid")]
        let asset_db_path = m.value_of("asset_db_path").map(PathBuf::from);

        let default_daemon_port = match network_type {
            #[cfg(not(feature = "liquid"))]
            Network::Bitcoin => 8332,
            #[cfg(not(feature = "liquid"))]
            Network::Testnet => 18332,
            #[cfg(not(feature = "liquid"))]
            Network::Testnet4 => 48332,
            #[cfg(not(feature = "liquid"))]
            Network::Regtest => 18443,
            #[cfg(not(feature = "liquid"))]
            Network::Signet => 38332,

            #[cfg(feature = "liquid")]
            Network::Liquid => 7041,
            #[cfg(feature = "liquid")]
            Network::LiquidTestnet | Network::LiquidRegtest => 7040,
        };
        let default_electrum_port = match network_type {
            #[cfg(not(feature = "liquid"))]
            Network::Bitcoin => 50001,
            #[cfg(not(feature = "liquid"))]
            Network::Testnet => 60001,
            #[cfg(not(feature = "liquid"))]
            Network::Testnet4 => 40001,
            #[cfg(not(feature = "liquid"))]
            Network::Regtest => 60401,
            #[cfg(not(feature = "liquid"))]
            Network::Signet => 60601,

            #[cfg(feature = "liquid")]
            Network::Liquid => 51000,
            #[cfg(feature = "liquid")]
            Network::LiquidTestnet => 51301,
            #[cfg(feature = "liquid")]
            Network::LiquidRegtest => 51401,
        };
        let default_http_port = match network_type {
            #[cfg(not(feature = "liquid"))]
            Network::Bitcoin => 3000,
            #[cfg(not(feature = "liquid"))]
            Network::Testnet => 3001,
            #[cfg(not(feature = "liquid"))]
            Network::Testnet4 => 3004,
            #[cfg(not(feature = "liquid"))]
            Network::Regtest => 3002,
            #[cfg(not(feature = "liquid"))]
            Network::Signet => 3003,

            #[cfg(feature = "liquid")]
            Network::Liquid => 3000,
            #[cfg(feature = "liquid")]
            Network::LiquidTestnet => 3001,
            #[cfg(feature = "liquid")]
            Network::LiquidRegtest => 3002,
        };
        let default_monitoring_port = match network_type {
            #[cfg(not(feature = "liquid"))]
            Network::Bitcoin => 4224,
            #[cfg(not(feature = "liquid"))]
            Network::Testnet => 14224,
            #[cfg(not(feature = "liquid"))]
            Network::Testnet4 => 44224,
            #[cfg(not(feature = "liquid"))]
            Network::Regtest => 24224,
            #[cfg(not(feature = "liquid"))]
            Network::Signet => 54224,

            #[cfg(feature = "liquid")]
            Network::Liquid => 34224,
            #[cfg(feature = "liquid")]
            Network::LiquidTestnet => 44324,
            #[cfg(feature = "liquid")]
            Network::LiquidRegtest => 44224,
        };

        let daemon_rpc_addr: SocketAddr = str_to_socketaddr(
            m.value_of("daemon_rpc_addr")
                .unwrap_or(&format!("127.0.0.1:{}", default_daemon_port)),
            "Bitcoin RPC",
        );
        let electrum_rpc_addr: SocketAddr = str_to_socketaddr(
            m.value_of("electrum_rpc_addr")
                .unwrap_or(&format!("127.0.0.1:{}", default_electrum_port)),
            "Electrum RPC",
        );
        let http_addr: SocketAddr = str_to_socketaddr(
            m.value_of("http_addr")
                .unwrap_or(&format!("127.0.0.1:{}", default_http_port)),
            "HTTP Server",
        );
        let zmq_addr: Option<SocketAddr> = m
            .value_of("zmq_addr")
            .map(|e| str_to_socketaddr(e, "ZMQ addr"));

        let http_socket_file: Option<PathBuf> = m.value_of("http_socket_file").map(PathBuf::from);
        let monitoring_addr: SocketAddr = str_to_socketaddr(
            m.value_of("monitoring_addr")
                .unwrap_or(&format!("127.0.0.1:{}", default_monitoring_port)),
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

        if let Some(network_subdir) = get_network_subdir(network_type) {
            daemon_dir.push(network_subdir);
        }
        let blocks_dir = m
            .value_of("blocks_dir")
            .map(PathBuf::from)
            .unwrap_or_else(|| daemon_dir.join("blocks"));
        let cookie = m.value_of("cookie").map(|s| s.to_owned());

        let electrum_banner = m.value_of("electrum_banner").map_or_else(
            || format!("Welcome to electrs-esplora {}", ELECTRS_VERSION),
            |s| s.into(),
        );

        #[cfg(feature = "electrum-discovery")]
        let electrum_public_hosts = m
            .value_of("electrum_public_hosts")
            .map(|s| serde_json::from_str(s).expect("invalid --electrum-public-hosts"));

        let mut log = stderrlog::new();
        log.verbosity(m.occurrences_of("verbosity") as usize);
        log.timestamp(if m.is_present("timestamp") {
            stderrlog::Timestamp::Millisecond
        } else {
            stderrlog::Timestamp::Off
        });
        log.init().expect("logging initialization failed");

        let config = Config {
            log,
            network_type,
            db_path,
            daemon_dir,
            blocks_dir,
            daemon_rpc_addr,
            daemon_parallelism: value_t_or_exit!(m, "daemon_parallelism", usize),
            cookie,
            utxos_limit: value_t_or_exit!(m, "utxos_limit", usize),
            electrum_rpc_addr,
            electrum_txs_limit: value_t_or_exit!(m, "electrum_txs_limit", usize),
            electrum_banner,
            electrum_rpc_logging: m
                .value_of("electrum_rpc_logging")
                .map(|option| RpcLogging::from(option)),
            http_addr,
            http_socket_file,
            monitoring_addr,
            jsonrpc_import: m.is_present("jsonrpc_import"),
            light_mode: m.is_present("light_mode"),
            address_search: m.is_present("address_search"),
            index_unspendables: m.is_present("index_unspendables"),
            cors: m.value_of("cors").map(|s| s.to_string()),
            precache_scripts: m.value_of("precache_scripts").map(|s| s.to_string()),
            initial_sync_compaction: m.is_present("initial_sync_compaction"),
            zmq_addr,

            #[cfg(feature = "liquid")]
            parent_network,
            #[cfg(feature = "liquid")]
            asset_db_path,

            #[cfg(feature = "electrum-discovery")]
            electrum_public_hosts,
            #[cfg(feature = "electrum-discovery")]
            electrum_announce: m.is_present("electrum_announce"),
            #[cfg(feature = "electrum-discovery")]
            tor_proxy: m.value_of("tor_proxy").map(|s| s.parse().unwrap()),
        };
        eprintln!("{:?}", config);
        config
    }

    pub fn cookie_getter(&self) -> Arc<dyn CookieGetter> {
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

#[derive(Debug, Clone)]
pub enum RpcLogging {
    Full,
    NoParams,
}

impl RpcLogging {
    pub fn options() -> Vec<String> {
        return vec!["full".to_string(), "no-params".to_string()];
    }
}

impl From<&str> for RpcLogging {
    fn from(option: &str) -> Self {
        match option {
            "full" => RpcLogging::Full,
            "no-params" => RpcLogging::NoParams,

            _ => panic!("unsupported RPC logging option: {:?}", option),
        }
    }
}

pub fn get_network_subdir(network: Network) -> Option<&'static str> {
    match network {
        #[cfg(not(feature = "liquid"))]
        Network::Bitcoin => None,
        #[cfg(not(feature = "liquid"))]
        Network::Testnet => Some("testnet3"),
        #[cfg(not(feature = "liquid"))]
        Network::Testnet4 => Some("testnet4"),
        #[cfg(not(feature = "liquid"))]
        Network::Regtest => Some("regtest"),
        #[cfg(not(feature = "liquid"))]
        Network::Signet => Some("signet"),

        #[cfg(feature = "liquid")]
        Network::Liquid => Some("liquidv1"),
        #[cfg(feature = "liquid")]
        Network::LiquidTestnet => Some("liquidtestnet"),
        #[cfg(feature = "liquid")]
        Network::LiquidRegtest => Some("liquidregtest"),
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
