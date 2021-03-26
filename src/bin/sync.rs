use anyhow::Result;

use electrs::{Config, Daemon, Tracker};

fn main() -> Result<()> {
    let config = Config::from_args();
    let daemon = Daemon::connect(&config)?;
    Tracker::new(&config)?.sync(&daemon)
}
