use clap::{App, Arg};
use std::env::home_dir;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use stderrlog;

use daemon::Network;

use errors::*;

fn read_cookie(daemon_dir: &Path) -> Result<String> {
    let mut path = daemon_dir.to_path_buf();
    path.push(".cookie");
    let contents = String::from_utf8(
        fs::read(&path).chain_err(|| format!("failed to read cookie from {:?}", path))?
    ).chain_err(|| "invalid cookie string")?;
    Ok(contents.trim().to_owned())
}

#[derive(Debug)]
pub struct Config {
    pub log: stderrlog::StdErrLog,
    pub network_type: Network,       // bitcoind JSONRPC endpoint
    pub db_path: PathBuf,            // RocksDB directory path
    pub daemon_dir: PathBuf,         // Bitcoind data directory
    pub cookie: String,              // for bitcoind JSONRPC authentication ("USER:PASSWORD")
    pub rpc_addr: SocketAddr,        // for serving Electrum clients
    pub monitoring_addr: SocketAddr, // for Prometheus monitoring
}

impl Config {
    pub fn from_args() -> Config {
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
                    .help("Directory to store index database (deafult: ./db/)")
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
                Arg::with_name("testnet")
                    .long("testnet")
                    .help("Connect to a testnet bitcoind instance"),
            )
            .get_matches();
        let network_type = match m.is_present("testnet") {
            false => Network::Mainnet,
            true => Network::Testnet,
        };
        let db_dir = Path::new(m.value_of("db_dir").unwrap_or("./db"));
        let mut daemon_dir = m.value_of("daemon_dir")
            .map(|p| PathBuf::from(p))
            .unwrap_or_else(|| {
                let mut default_dir = home_dir().expect("no homedir");
                default_dir.push(".bitcoin");
                default_dir
            });
        if let Network::Testnet = network_type {
            daemon_dir.push("testnet3");
        }
        let cookie = m.value_of("cookie")
            .map(|s| s.to_owned())
            .unwrap_or_else(|| read_cookie(&daemon_dir).unwrap());

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
            db_path: match network_type {
                Network::Mainnet => db_dir.join("mainnet"),
                Network::Testnet => db_dir.join("testnet"),
            },
            daemon_dir,
            cookie,
            rpc_addr: match network_type {
                Network::Mainnet => "127.0.0.1:50001",
                Network::Testnet => "127.0.0.1:60001",
            }.parse()
                .unwrap(),
            monitoring_addr: "127.0.0.1:42024".parse().unwrap(),
        };
        eprintln!("{:?}", config);
        config
    }
}
