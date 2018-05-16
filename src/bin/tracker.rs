extern crate indexrs;
extern crate simplelog;
#[macro_use]
extern crate log;

use indexrs::{daemon, mempool};
use std::thread;
use std::time::{Duration, Instant};

fn main() {
    use simplelog::*;
    let mut cfg = Config::default();
    cfg.time_format = Some("%F %H:%M:%S%.3f");
    CombinedLogger::init(vec![
        TermLogger::new(LevelFilter::Debug, cfg.clone()).unwrap(),
    ]).unwrap();

    let daemon = daemon::Daemon::new("localhost:8332");
    let mut tracker = mempool::Tracker::new(&daemon);
    loop {
        let t = Instant::now();
        tracker.update_from_daemon().unwrap();
        let dt = t.elapsed();
        info!(
            "update took {:.3} ms",
            (dt.as_secs() as f64 + 1e-9f64 * dt.subsec_nanos() as f64) * 1e3
        );
        thread::sleep(Duration::from_secs(1));
    }
}
