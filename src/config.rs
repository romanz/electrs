use bitcoin::network::constants::{Magic, Network};
use bitcoincore_rpc::Auth;
use dirs_next::home_dir;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::str::FromStr;

use std::env::consts::{ARCH, OS};
use std::time::Duration;

pub const ELECTRS_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_SERVER_ADDRESS: [u8; 4] = [127, 0, 0, 1]; // by default, serve on IPv4 localhost

mod internal {
    #![allow(clippy::enum_variant_names)]
    #![allow(clippy::unnecessary_lazy_evaluations)]
    #![allow(clippy::useless_conversion)]

    include!(concat!(env!("OUT_DIR"), "/configure_me_config.rs"));
}

/// A simple error type representing invalid UTF-8 input.
pub struct InvalidUtf8(OsString);

impl fmt::Display for InvalidUtf8 {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?} isn't a valid UTF-8 sequence", self.0)
    }
}

/// An error that might happen when resolving an address
pub enum AddressError {
    ResolvError { addr: String, err: std::io::Error },
    NoAddrError(String),
}

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            AddressError::ResolvError { addr, err } => {
                write!(f, "Failed to resolve address {}: {}", addr, err)
            }
            AddressError::NoAddrError(addr) => write!(f, "No address found for {}", addr),
        }
    }
}

/// Newtype for an address that is parsed as `String`
///
/// The main point of this newtype is to provide better description than what `String` type
/// provides.
#[derive(Deserialize)]
pub struct ResolvAddr(String);

impl ::configure_me::parse_arg::ParseArg for ResolvAddr {
    type Error = InvalidUtf8;

    fn parse_arg(arg: &OsStr) -> std::result::Result<Self, Self::Error> {
        Self::parse_owned_arg(arg.to_owned())
    }

    fn parse_owned_arg(arg: OsString) -> std::result::Result<Self, Self::Error> {
        arg.into_string().map_err(InvalidUtf8).map(ResolvAddr)
    }

    fn describe_type<W: fmt::Write>(mut writer: W) -> fmt::Result {
        write!(writer, "a network address (will be resolved if needed)")
    }
}

impl ResolvAddr {
    /// Resolves the address.
    fn resolve(self) -> std::result::Result<SocketAddr, AddressError> {
        match self.0.to_socket_addrs() {
            Ok(mut iter) => iter.next().ok_or(AddressError::NoAddrError(self.0)),
            Err(err) => Err(AddressError::ResolvError { addr: self.0, err }),
        }
    }

    /// Resolves the address, but prints error and exits in case of failure.
    fn resolve_or_exit(self) -> SocketAddr {
        self.resolve().unwrap_or_else(|err| {
            eprintln!("Error: {}", err);
            std::process::exit(1)
        })
    }
}

/// This newtype implements `ParseArg` for `Network`.
#[derive(Deserialize)]
pub struct BitcoinNetwork(Network);

impl Default for BitcoinNetwork {
    fn default() -> Self {
        BitcoinNetwork(Network::Bitcoin)
    }
}

impl FromStr for BitcoinNetwork {
    type Err = <Network as FromStr>::Err;

    fn from_str(string: &str) -> std::result::Result<Self, Self::Err> {
        Network::from_str(string).map(BitcoinNetwork)
    }
}

impl ::configure_me::parse_arg::ParseArgFromStr for BitcoinNetwork {
    fn describe_type<W: fmt::Write>(mut writer: W) -> fmt::Result {
        write!(writer, "either 'bitcoin', 'testnet', 'regtest' or 'signet'")
    }
}

impl From<BitcoinNetwork> for Network {
    fn from(network: BitcoinNetwork) -> Network {
        network.0
    }
}

/// Parsed and post-processed configuration
#[derive(Debug)]
pub struct Config {
    // See below for the documentation of each field:
    pub network: Network,
    pub db_path: PathBuf,
    pub daemon_dir: PathBuf,
    pub daemon_auth: SensitiveAuth,
    pub daemon_rpc_addr: SocketAddr,
    pub daemon_p2p_addr: SocketAddr,
    pub electrum_rpc_addr: SocketAddr,
    pub monitoring_addr: SocketAddr,
    pub wait_duration: Duration,
    pub jsonrpc_timeout: Duration,
    pub index_batch_size: usize,
    pub index_lookup_limit: Option<usize>,
    pub reindex_last_blocks: usize,
    pub auto_reindex: bool,
    pub ignore_mempool: bool,
    pub sync_once: bool,
    pub disable_electrum_rpc: bool,
    pub server_banner: String,
    pub signet_magic: Magic,
    pub args: Vec<String>,
}

pub struct SensitiveAuth(pub Auth);

impl SensitiveAuth {
    pub(crate) fn get_auth(&self) -> Auth {
        self.0.clone()
    }
}

impl fmt::Debug for SensitiveAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Auth::UserPass(ref user, _) => f
                .debug_tuple("UserPass")
                .field(&user)
                .field(&"<sensitive>")
                .finish(),
            _ => write!(f, "{:?}", self.0),
        }
    }
}

/// Returns default daemon directory
fn default_daemon_dir() -> PathBuf {
    let mut home = home_dir().unwrap_or_else(|| {
        eprintln!("Error: unknown home directory");
        std::process::exit(1)
    });
    home.push(".bitcoin");
    home
}

fn default_config_files() -> Vec<OsString> {
    let mut files = vec![OsString::from("electrs.toml")]; // cwd
    if let Some(mut path) = home_dir() {
        path.extend(&[".electrs", "config.toml"]);
        files.push(OsString::from(path)) // home directory
    }
    files.push(OsString::from("/etc/electrs/config.toml")); // system-wide
    files
}

impl Config {
    /// Parses args, env vars, config files and post-processes them
    pub fn from_args() -> Config {
        use internal::ResultExt;

        let (mut config, args) =
            internal::Config::including_optional_config_files(default_config_files())
                .unwrap_or_exit();

        fn unsupported_network(network: Network) -> ! {
            eprintln!("Error: unsupported network: {}", network);
            std::process::exit(1);
        }

        let db_subdir = match config.network {
            Network::Bitcoin => "bitcoin",
            Network::Testnet => "testnet",
            Network::Regtest => "regtest",
            Network::Signet => "signet",
            unsupported => unsupported_network(unsupported),
        };

        config.db_dir.push(db_subdir);

        let default_daemon_rpc_port = match config.network {
            Network::Bitcoin => 8332,
            Network::Testnet => 18332,
            Network::Regtest => 18443,
            Network::Signet => 38332,
            unsupported => unsupported_network(unsupported),
        };
        let default_daemon_p2p_port = match config.network {
            Network::Bitcoin => 8333,
            Network::Testnet => 18333,
            Network::Regtest => 18444,
            Network::Signet => 38333,
            unsupported => unsupported_network(unsupported),
        };
        let default_electrum_port = match config.network {
            Network::Bitcoin => 50001,
            Network::Testnet => 60001,
            Network::Regtest => 60401,
            Network::Signet => 60601,
            unsupported => unsupported_network(unsupported),
        };
        let default_monitoring_port = match config.network {
            Network::Bitcoin => 4224,
            Network::Testnet => 14224,
            Network::Regtest => 24224,
            Network::Signet => 34224,
            unsupported => unsupported_network(unsupported),
        };

        let magic = match (config.network, config.signet_magic) {
            (Network::Signet, Some(magic)) => magic.parse().unwrap_or_else(|error| {
                eprintln!(
                    "Error: signet magic '{}' is not a valid hex string: {}",
                    magic, error
                );
                std::process::exit(1);
            }),
            (network, None) => network.magic(),
            (_, Some(_)) => {
                eprintln!("Error: signet magic only available on signet");
                std::process::exit(1);
            }
        };

        let daemon_rpc_addr: SocketAddr = config.daemon_rpc_addr.map_or(
            (DEFAULT_SERVER_ADDRESS, default_daemon_rpc_port).into(),
            ResolvAddr::resolve_or_exit,
        );
        let daemon_p2p_addr: SocketAddr = config.daemon_p2p_addr.map_or(
            (DEFAULT_SERVER_ADDRESS, default_daemon_p2p_port).into(),
            ResolvAddr::resolve_or_exit,
        );
        let electrum_rpc_addr: SocketAddr = config.electrum_rpc_addr.map_or(
            (DEFAULT_SERVER_ADDRESS, default_electrum_port).into(),
            ResolvAddr::resolve_or_exit,
        );
        #[cfg(not(feature = "metrics"))]
        {
            if config.monitoring_addr.is_some() {
                eprintln!("Error: enable \"metrics\" feature to specify monitoring_addr");
                std::process::exit(1);
            }
        }
        let monitoring_addr: SocketAddr = config.monitoring_addr.map_or(
            (DEFAULT_SERVER_ADDRESS, default_monitoring_port).into(),
            ResolvAddr::resolve_or_exit,
        );

        match config.network {
            Network::Bitcoin => (),
            Network::Testnet => config.daemon_dir.push("testnet3"),
            Network::Regtest => config.daemon_dir.push("regtest"),
            Network::Signet => config.daemon_dir.push("signet"),
            unsupported => unsupported_network(unsupported),
        }

        let daemon_dir = &config.daemon_dir;
        let daemon_auth = SensitiveAuth(match (config.auth, config.cookie_file) {
            (None, None) => Auth::CookieFile(daemon_dir.join(".cookie")),
            (None, Some(cookie_file)) => Auth::CookieFile(cookie_file),
            (Some(auth), None) => {
                let parts: Vec<&str> = auth.splitn(2, ':').collect();
                if parts.len() != 2 {
                    eprintln!("Error: auth cookie doesn't contain colon");
                    std::process::exit(1);
                }
                Auth::UserPass(parts[0].to_owned(), parts[1].to_owned())
            }
            (Some(_), Some(_)) => {
                eprintln!("Error: ambiguous configuration - auth and cookie_file can't be specified at the same time");
                std::process::exit(1);
            }
        });

        if config.verbose > 0 {
            eprintln!("Error: please use `log_filters` to set logging verbosity",);
            std::process::exit(1);
        }
        let log_filters = config.log_filters;

        let index_lookup_limit = match config.index_lookup_limit {
            0 => None,
            _ => Some(config.index_lookup_limit),
        };

        if config.jsonrpc_timeout_secs <= config.wait_duration_secs {
            eprintln!(
                "Error: jsonrpc_timeout_secs ({}) must be higher than wait_duration_secs ({})",
                config.jsonrpc_timeout_secs, config.wait_duration_secs
            );
            std::process::exit(1);
        }

        if config.version {
            println!("v{}", ELECTRS_VERSION);
            std::process::exit(0);
        }

        let config = Config {
            network: config.network,
            db_path: config.db_dir,
            daemon_dir: config.daemon_dir,
            daemon_auth,
            daemon_rpc_addr,
            daemon_p2p_addr,
            electrum_rpc_addr,
            monitoring_addr,
            wait_duration: Duration::from_secs(config.wait_duration_secs),
            jsonrpc_timeout: Duration::from_secs(config.jsonrpc_timeout_secs),
            index_batch_size: config.index_batch_size,
            index_lookup_limit,
            reindex_last_blocks: config.reindex_last_blocks,
            auto_reindex: config.auto_reindex,
            ignore_mempool: config.ignore_mempool,
            sync_once: config.sync_once,
            disable_electrum_rpc: config.disable_electrum_rpc,
            server_banner: config.server_banner,
            signet_magic: magic,
            args: args.map(|a| a.into_string().unwrap()).collect(),
        };
        eprintln!(
            "Starting electrs {} on {} {} with {:?}",
            ELECTRS_VERSION, ARCH, OS, config
        );
        let mut builder = env_logger::Builder::from_default_env();
        builder.default_format().format_timestamp_millis();
        if let Some(log_filters) = &log_filters {
            builder.parse_filters(log_filters);
        }
        builder.init();

        config
    }
}

#[cfg(test)]
mod tests {
    use super::{Auth, SensitiveAuth};
    use std::path::Path;

    #[test]
    fn test_auth_debug() {
        let auth = Auth::None;
        assert_eq!(format!("{:?}", SensitiveAuth(auth)), "None");

        let auth = Auth::CookieFile(Path::new("/foo/bar/.cookie").to_path_buf());
        assert_eq!(
            format!("{:?}", SensitiveAuth(auth)),
            "CookieFile(\"/foo/bar/.cookie\")"
        );

        let auth = Auth::UserPass("user".to_owned(), "pass".to_owned());
        assert_eq!(
            format!("{:?}", SensitiveAuth(auth)),
            "UserPass(\"user\", \"<sensitive>\")"
        );
    }
}
