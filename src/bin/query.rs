#[macro_use]
extern crate log;

use anyhow::Result;
use bitcoin::{Address, Amount};

use std::collections::BTreeMap;
use std::str::FromStr;

use electrs::{Balance, Cache, Config, Daemon, ScriptHash, Status, Tracker};

fn main() -> Result<()> {
    let config = Config::from_args();
    let addresses = config
        .args
        .iter()
        .map(|a| Address::from_str(a).expect("invalid address"));

    let cache = Cache::default();
    let daemon = Daemon::connect(&config)?;
    let mut tracker = Tracker::new(&config)?;
    let mut map: BTreeMap<Address, Status> = addresses
        .map(|addr| {
            let status = Status::new(ScriptHash::new(&addr.script_pubkey()));
            (addr, status)
        })
        .collect();

    loop {
        tracker.sync(&daemon)?;
        let mut total = Amount::ZERO;
        for (addr, status) in map.iter_mut() {
            tracker.update_status(status, &daemon, &cache)?;
            let balance = tracker.get_balance(status, &cache);
            if balance != Balance::default() {
                info!("{} has {}", addr, balance.confirmed());
            }
            total += balance.confirmed();
        }
        info!("total: {}", total);
        std::thread::sleep(config.wait_duration);
    }
}
