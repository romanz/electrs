use anyhow::{Context, Result};
use hyper::server::{Handler, Listening, Request, Response, Server};
use prometheus::{
    self, process_collector::ProcessCollector, Encoder, HistogramOpts, HistogramVec, Registry,
};

use std::net::SocketAddr;

pub struct Metrics {
    reg: Registry,
    listen: Listening,
}

impl Drop for Metrics {
    fn drop(&mut self) {
        debug!("closing Prometheus server");
        if let Err(e) = self.listen.close() {
            warn!("failed to stop Prometheus server: {}", e);
        }
    }
}

#[derive(Clone)]
pub struct Histogram {
    hist: HistogramVec,
}

impl Histogram {
    pub fn observe_size(&self, label: &str, value: usize) {
        self.hist.with_label_values(&[label]).observe(value as f64);
    }

    pub fn observe_duration<F, T>(&self, label: &str, func: F) -> T
    where
        F: FnOnce() -> T,
    {
        self.hist
            .with_label_values(&[label])
            .observe_closure_duration(func)
    }
}

struct RegistryHandler {
    reg: Registry,
}

impl RegistryHandler {
    fn gather(&self) -> Result<Vec<u8>> {
        let mut buffer = vec![];
        prometheus::TextEncoder::new()
            .encode(&self.reg.gather(), &mut buffer)
            .context("failed to encode metrics")?;
        Ok(buffer)
    }
}

impl Handler for RegistryHandler {
    fn handle(&self, req: Request, res: Response) {
        trace!("{} {}", req.method, req.uri);
        let buffer = self.gather().expect("failed to gather metrics");
        res.send(&buffer).expect("send failed");
    }
}

impl Metrics {
    pub fn new(addr: SocketAddr) -> Result<Self> {
        let reg = Registry::new();

        reg.register(Box::new(ProcessCollector::for_self()))
            .expect("failed to register ProcessCollector");

        let listen = Server::http(addr)?
            .handle(RegistryHandler { reg: reg.clone() })
            .with_context(|| format!("failed to serve on {}", addr))?;
        info!("serving Prometheus metrics on {}", addr);
        Ok(Self { reg, listen })
    }

    pub fn histogram_vec(&self, name: &str, desc: &str, labels: &[&str]) -> Histogram {
        let opts = HistogramOpts::new(name, desc);
        let hist = HistogramVec::new(opts, labels).unwrap();
        self.reg
            .register(Box::new(hist.clone()))
            .expect("failed to register Histogram");
        Histogram { hist }
    }
}
