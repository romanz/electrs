extern crate electrs;

#[macro_use]
extern crate log;

use electrs::config::Config;
use electrs::notify;

fn main() {
    let _ = Config::from_args();
    let rx = notify::run().into_receiver();
    for msg in rx.iter() {
        info!("{:?}", msg)
    }
}
