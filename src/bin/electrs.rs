#![recursion_limit = "256"]

use anyhow::{Context, Result};
use electrs::{server, Config, Rpc, Tracker};

fn main() -> Result<()> {
    let config = Config::from_args();
    let rpc = Rpc::new(&config, Tracker::new(&config)?)?;
    if config.sync_once {
        return Ok(());
    }
    server::run(&config, rpc).context("server failed")
}
