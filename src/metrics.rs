use page_size;
use prometheus::{self, Encoder};
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::thread;
use std::time::Duration;
use tiny_http;

pub use prometheus::{
    GaugeVec, Histogram, HistogramOpts, HistogramTimer, HistogramVec, IntCounter as Counter,
    IntCounterVec as CounterVec, IntGauge as Gauge, Opts as MetricOpts,
};

use util::spawn_thread;

use errors::*;

pub struct Metrics {
    reg: prometheus::Registry,
    addr: SocketAddr,
}

impl Metrics {
    pub fn new(addr: SocketAddr) -> Metrics {
        Metrics {
            reg: prometheus::Registry::new(),
            addr,
        }
    }

    pub fn counter(&self, opts: prometheus::Opts) -> Counter {
        let c = Counter::with_opts(opts).unwrap();
        self.reg.register(Box::new(c.clone())).unwrap();
        c
    }

    pub fn counter_vec(&self, opts: prometheus::Opts, labels: &[&str]) -> CounterVec {
        let c = CounterVec::new(opts, labels).unwrap();
        self.reg.register(Box::new(c.clone())).unwrap();
        c
    }

    pub fn gauge(&self, opts: prometheus::Opts) -> Gauge {
        let g = Gauge::with_opts(opts).unwrap();
        self.reg.register(Box::new(g.clone())).unwrap();
        g
    }

    pub fn gauge_vec(&self, opts: prometheus::Opts, labels: &[&str]) -> GaugeVec {
        let g = GaugeVec::new(opts, labels).unwrap();
        self.reg.register(Box::new(g.clone())).unwrap();
        g
    }

    pub fn histogram(&self, opts: prometheus::HistogramOpts) -> Histogram {
        let h = Histogram::with_opts(opts).unwrap();
        self.reg.register(Box::new(h.clone())).unwrap();
        h
    }

    pub fn histogram_vec(&self, opts: prometheus::HistogramOpts, labels: &[&str]) -> HistogramVec {
        let h = HistogramVec::new(opts, labels).unwrap();
        self.reg.register(Box::new(h.clone())).unwrap();
        h
    }

    pub fn start(&self) {
        let server = tiny_http::Server::http(self.addr).expect(&format!(
            "failed to start monitoring HTTP server at {}",
            self.addr
        ));
        start_process_exporter(&self);
        let reg = self.reg.clone();
        spawn_thread("metrics", move || loop {
            if let Err(e) = handle_request(&reg, server.recv()) {
                error!("http error: {}", e);
            }
        });
    }
}

fn handle_request(
    reg: &prometheus::Registry,
    request: io::Result<tiny_http::Request>,
) -> io::Result<()> {
    let request = request?;
    let mut buffer = vec![];
    prometheus::TextEncoder::new()
        .encode(&reg.gather(), &mut buffer)
        .unwrap();
    let response = tiny_http::Response::from_data(buffer);
    request.respond(response)
}

struct Stats {
    rss: usize,
}

fn parse_stats(path: &Path) -> Result<Stats> {
    let value = fs::read_to_string(path).chain_err(|| "failed to read stats")?;
    let parts: Vec<&str> = value.split_whitespace().collect();
    let page_size = page_size::get();

    let rss_in_pages: usize = parts
        .get(23)
        .chain_err(|| "missing value")?
        .parse::<usize>()
        .chain_err(|| "invalid value")?;

    Ok(Stats {
        rss: rss_in_pages * page_size,
    })
}

fn start_process_exporter(metrics: &Metrics) {
    let rss = metrics.gauge(MetricOpts::new(
        "process_memory_rss",
        "Resident memory size [bytes]",
    ));
    let path = Path::new("/proc/self/stat");
    spawn_thread("exporter", move || loop {
        match parse_stats(path) {
            Ok(stats) => rss.set(stats.rss as i64),
            Err(e) => warn!("failed to parse {:?}: {}", path, e),
        }
        thread::sleep(Duration::from_secs(5));
    });
}
