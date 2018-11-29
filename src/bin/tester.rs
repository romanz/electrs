extern crate bitcoin;
extern crate error_chain;
#[macro_use]
extern crate log;

extern crate electrs;

use error_chain::ChainedError;
use std::path::Path;
use std::process;
use std::str::FromStr;

use bitcoin::util::address::Address;

use electrs::{
    config::Config, daemon::Daemon, errors::*, metrics::Metrics, new_index, signal::Waiter,
    util::HeaderList,
};

fn run_server(config: Config) -> Result<()> {
    let signal = Waiter::new();
    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();

    let daemon = Daemon::new(
        &config.daemon_dir,
        config.daemon_rpc_addr,
        config.cookie_getter(),
        config.network_type,
        signal.clone(),
        &metrics,
    )?;
    let mut indexer = new_index::Indexer::open(Path::new("newindex-db"));
    let headers = HeaderList::empty();
    let headers = indexer.update(&daemon, headers)?;
    let addr = Address::from_str("msRnv37GmMXU86EbPZTkGCCqYw1zUZX6v6").unwrap();
    for txid in indexer.history(&addr.script_pubkey()).keys() {
        info!("{}", txid);
    }
    Ok(())
}

fn main() {
    let config = Config::from_args();
    if let Err(e) = run_server(config) {
        error!("server failed: {}", e.display_chain());
        process::exit(1);
    }
}
