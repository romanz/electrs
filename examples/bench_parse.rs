extern crate electrs;

#[macro_use]
extern crate log;

extern crate error_chain;

use std::path::Path;

use electrs::{
    bulk::Parser, config::Config, daemon::Daemon, errors::*, metrics::Metrics,
    store::{DBStore, StoreOptions, WriteStore},
};

use error_chain::ChainedError;

fn run(config: Config) -> Result<()> {
    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();

    let daemon = Daemon::new(
        &config.daemon_dir,
        &config.cookie,
        config.network_type,
        &metrics,
    )?;
    let store = DBStore::open(Path::new("./test-db"), StoreOptions { bulk_import: true });
    let parser = Parser::new(&daemon, &metrics)?;
    for path in daemon.list_blk_files()? {
        let blob = parser.read_blkfile(&path)?;
        let rows = parser.index_blkfile(blob)?;
        store.write(rows);
    }
    Ok(())
}

fn main() {
    if let Err(e) = run(Config::from_args()) {
        error!("{}", e.display_chain());
    }
}
