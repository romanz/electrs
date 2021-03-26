#![recursion_limit = "256"]

use anyhow::{Context, Result};
use electrs::{server, Config, Daemon, Rpc, Tracker};

fn main() -> Result<()> {
    let config = Config::from_args();
    let mut tracker = Tracker::new(&config)?;
    tracker
        .sync(&Daemon::connect(&config)?)
        .context("initial sync failed")?;

    // re-connect after initial sync (due to possible timeout during compaction)
    server::run(&config, Rpc::new(&config, tracker)?).context("server failed")
}
