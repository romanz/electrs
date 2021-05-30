use anyhow::Result;

use electrs::{Config, Daemon, Tracker};

fn main() -> Result<()> {
    let config = Config::from_args();
    let daemon = Daemon::connect(&config)?;
    let mut tracker = Tracker::new(&config)?;
    loop {
        tracker.sync(&daemon)?;
        std::thread::sleep(config.wait_duration);
    }
}
