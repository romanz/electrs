extern crate electrs;

#[macro_use]
extern crate log;

extern crate error_chain;

use electrs::{
    bulk, config::Config, daemon::Daemon, errors::*, metrics::Metrics,
    store::{DBStore},
};

use error_chain::ChainedError;

fn run(config: Config) -> Result<()> {
    if config.db_path.exists() {
        panic!(
            "DB {:?} must not exist when running this benchmark!",
            config.db_path
        );
    }
    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();
    let daemon = Daemon::new(
        &config.daemon_dir,
        &config.cookie,
        config.network_type,
        &metrics,
    )?;
    let store = DBStore::open(&config.db_path);
    bulk::index(&daemon, &metrics, store)?;
    Ok(())
}

fn main() {
    if let Err(e) = run(Config::from_args()) {
        error!("{}", e.display_chain());
    }
}
