extern crate time;

use std::fmt::Write;
use std::collections::HashMap;
use self::time::{Duration, PreciseTime};

pub struct Timer {
    durations: HashMap<String, Duration>,
    start: Option<PreciseTime>,
    name: String,
}

impl Timer {
    pub fn new() -> Timer {
        Timer {
            durations: HashMap::new(),
            start: None,
            name: String::from(""),
        }
    }

    pub fn start(&mut self, name: &str) {
        self.start = Some(self.stop());
        self.name = name.to_string();
    }

    pub fn stop(&mut self) -> PreciseTime {
        let now = PreciseTime::now();
        if let Some(start) = self.start {
            let duration = self.durations
                .entry(self.name.to_string())   // ???
                .or_insert(Duration::zero());
            *duration = *duration + start.to(now);
        }
        self.start = None;
        now
    }

    pub fn stats(&self) -> String {
        let mut s = String::new();
        let mut total = 0f64;
        for (k, v) in self.durations.iter() {
            let t = v.num_milliseconds() as f64 / 1e3;
            total += t;
            write!(&mut s, "{}: {:.2}s ", k, t).unwrap();
        }
        write!(&mut s, "total: {:.2}s", total).unwrap();
        return s;
    }
}
