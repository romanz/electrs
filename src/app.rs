use argparse::{ArgumentParser, Store, StoreTrue};
use chan;
use chan_signal;
use error_chain::ChainedError;
use simplelog::LevelFilter;
use std::fs::OpenOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use daemon::Network;
use {daemon, index, query, rpc, store};

use errors::*;

#[derive(Debug)]
struct Config {
    log_file: String,
    log_level: LevelFilter,
    restart: bool,
    network_type: Network, // bitcoind JSONRPC endpoint
    db_path: &'static str, // RocksDB directory path
    rpc_addr: SocketAddr,  // for serving Electrum clients
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
        Config {
            log_file,
            log_level: if verbose {
                LevelFilter::Debug
            } else {
                LevelFilter::Info
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
        }
    }
}

pub struct App {
    store: store::DBStore,
    index: index::Index,
    daemon: daemon::Daemon,
}

impl App {
    pub fn write_store(&self) -> &store::WriteStore {
        &self.store
    }
    pub fn read_store(&self) -> &store::ReadStore {
        &self.store
    }
    pub fn index(&self) -> &index::Index {
        &self.index
    }
    pub fn daemon(&self) -> &daemon::Daemon {
        &self.daemon
    }
}

struct Waiter {
    signal: chan::Receiver<chan_signal::Signal>,
    duration: Duration,
}

impl Waiter {
    fn new(duration: Duration) -> Waiter {
        let signal = chan_signal::notify(&[chan_signal::Signal::INT]);
        Waiter { signal, duration }
    }
    fn wait(&self) -> Option<chan_signal::Signal> {
        let signal = &self.signal;
        let timeout = chan::after(self.duration);
        let result;
        chan_select! {
            signal.recv() -> sig => { result = sig; },
            timeout.recv() => { result = None; },
        }
        result
    }
}

fn run_server(config: &Config) -> Result<()> {
    let signal = Waiter::new(Duration::from_secs(5));
    let daemon = daemon::Daemon::new(config.network_type)?;
    debug!("{:?}", daemon.getblockchaininfo()?);

    let store = store::DBStore::open(
        config.db_path,
        store::StoreOptions {
            // compact manually after the first run has finished successfully
            auto_compact: false,
        },
    );
    let index = index::Index::load(&store);
    let mut tip = index.update(&store, &daemon)?;
    store.compact_if_needed();
    drop(store); // to be re-opened soon

    let store = store::DBStore::open(config.db_path, store::StoreOptions { auto_compact: true });
    let app = Arc::new(App {
        store,
        index,
        daemon,
    });

    let query = Arc::new(query::Query::new(app.clone()));
    rpc::start(&config.rpc_addr, query.clone());
    while let None = signal.wait() {
        query.update_mempool()?;
        if tip == app.daemon().getbestblockhash()? {
            continue;
        }
        tip = app.index().update(app.write_store(), app.daemon())?;
    }
    info!("closing server");
    Ok(())
}

fn setup_logging(config: &Config) {
    use simplelog::*;
    let mut cfg = Config::default();
    cfg.time_format = Some("%F %H:%M:%S%.3f");
    let mut loggers = Vec::<Box<SharedLogger>>::new();
    loggers.push(TermLogger::new(config.log_level, cfg.clone()).unwrap());
    if !config.log_file.is_empty() {
        loggers.push(WriteLogger::new(
            LevelFilter::Debug,
            cfg.clone(),
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&config.log_file)
                .unwrap(),
        ));
    }
    CombinedLogger::init(loggers).unwrap();
    info!("config: {:?}", config);
}

struct Repeat {
    do_restart: bool,
    iter_count: usize,
}

impl Repeat {
    fn new(config: &Config) -> Repeat {
        Repeat {
            do_restart: config.restart,
            iter_count: 0,
        }
    }
}

impl Iterator for Repeat {
    type Item = ();

    fn next(&mut self) -> Option<()> {
        self.iter_count += 1;
        if self.iter_count == 1 {
            return Some(()); // don't sleep before 1st iteration
        }
        thread::sleep(Duration::from_secs(1));
        if self.do_restart {
            Some(())
        } else {
            None
        }
    }
}

pub fn main() {
    let config = Config::from_args();
    setup_logging(&config);
    for _ in Repeat::new(&config) {
        match run_server(&config) {
            Ok(_) => break,
            Err(e) => error!("{}", e.display_chain()),
        }
    }
}
