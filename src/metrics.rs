#[cfg(feature = "metrics")]
mod metrics_impl {
    use anyhow::{Context, Result};
    use prometheus::process_collector::ProcessCollector;
    use prometheus::{self, Encoder, HistogramOpts, HistogramVec, Registry};
    use tiny_http::{Response, Server};

    use std::net::SocketAddr;

    use crate::thread::spawn;

    pub struct Metrics {
        reg: Registry,
    }

    impl Metrics {
        pub fn new(addr: SocketAddr) -> Result<Self> {
            let reg = Registry::new();

            reg.register(Box::new(ProcessCollector::for_self()))
                .expect("failed to register ProcessCollector");

            let result = Self { reg };
            let reg = result.reg.clone();
            spawn("metrics", move || {
                let server = Server::http(addr).unwrap();
                for request in server.incoming_requests() {
                    let mut buffer = vec![];
                    prometheus::TextEncoder::new()
                        .encode(&reg.gather(), &mut buffer)
                        .context("failed to encode metrics")?;
                    request
                        .respond(Response::from_data(buffer))
                        .context("failed to send HTTP response")?;
                }
                Ok(())
            });

            info!("serving Prometheus metrics on {}", addr);
            Ok(result)
        }

        pub fn histogram_vec(&self, name: &str, desc: &str, label: &str) -> Histogram {
            let opts = HistogramOpts::new(name, desc);
            let hist = HistogramVec::new(opts, &[label]).unwrap();
            self.reg
                .register(Box::new(hist.clone()))
                .expect("failed to register Histogram");
            Histogram { hist }
        }
    }

    #[derive(Clone)]
    pub struct Histogram {
        hist: HistogramVec,
    }

    impl Histogram {
        pub fn observe(&self, label: &str, value: usize) {
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
}

#[cfg(feature = "metrics")]
pub use metrics_impl::{Histogram, Metrics};

#[cfg(not(feature = "metrics"))]
mod metrics_fake {
    use anyhow::Result;

    use std::net::SocketAddr;

    pub struct Metrics {}

    impl Metrics {
        pub fn new(_addr: SocketAddr) -> Result<Self> {
            debug!("metrics collection is disabled");
            Ok(Self {})
        }

        pub fn histogram_vec(&self, _name: &str, _desc: &str, _label: &str) -> Histogram {
            Histogram {}
        }
    }

    #[derive(Clone)]
    pub struct Histogram {}

    impl Histogram {
        pub fn observe(&self, _label: &str, _value: usize) {}

        pub fn observe_duration<F, T>(&self, _label: &str, func: F) -> T
        where
            F: FnOnce() -> T,
        {
            func()
        }
    }
}

#[cfg(not(feature = "metrics"))]
pub use metrics_fake::{Histogram, Metrics};
