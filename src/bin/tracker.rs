extern crate indexrs;
extern crate simplelog;
#[macro_use]
extern crate log;

use indexrs::{daemon, mempool};
use std::thread;
use std::time::Duration;

fn main() {
    use simplelog::*;
    let mut cfg = Config::default();
    cfg.time_format = Some("%F %H:%M:%S%.3f");
    CombinedLogger::init(vec![
        TermLogger::new(LevelFilter::Debug, cfg.clone()).unwrap(),
    ]).unwrap();

    let daemon = daemon::Daemon::new(daemon::Network::Mainnet);
    let mut tracker = mempool::Tracker::new();
    loop {
        tracker.update(&daemon).unwrap();
        let h = tracker.fee_histogram();
        let mut total_vsize = 0;
        let mut limit = 0;
        for (fee, vsize) in h {
            total_vsize += vsize;
            let new_limit = total_vsize as usize / 1_000_000;
            if limit != new_limit {
                limit = new_limit;
                info!(
                    "{:.2}MB has fee >= {:.1} sat/vbyte",
                    total_vsize as f32 / 1e6,
                    fee
                );
            }
        }
        info!("{:.2}MB total size", total_vsize as f32 / 1e6);
        thread::sleep(Duration::from_secs(10));
    }
}
