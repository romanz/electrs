#[cfg(feature = "metrics")]
mod metrics_impl {
    use anyhow::{Context, Result};

    #[cfg(feature = "metrics_process")]
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

            #[cfg(feature = "metrics_process")]
            reg.register(Box::new(ProcessCollector::for_self()))
                .expect("failed to register ProcessCollector");

            let result = Self { reg };
            let reg = result.reg.clone();

            let server = match Server::http(addr) {
                Ok(server) => server,
                Err(err) => bail!("failed to start HTTP server on {}: {}", addr, err),
            };

            spawn("metrics", move || {
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

        pub fn histogram_vec(
            &self,
            name: &str,
            desc: &str,
            label: &str,
            buckets: Vec<f64>,
        ) -> Histogram {
            let name = String::from("electrs_") + name;
            let opts = HistogramOpts::new(name, desc).buckets(buckets);
            let hist = HistogramVec::new(opts, &[label]).unwrap();
            self.reg
                .register(Box::new(hist.clone()))
                .expect("failed to register Histogram");
            Histogram { hist }
        }

        pub fn gauge(&self, name: &str, desc: &str, label: &str) -> Gauge {
            let name = String::from("electrs_") + name;
            let opts = prometheus::Opts::new(name, desc);
            let gauge = prometheus::GaugeVec::new(opts, &[label]).unwrap();
            self.reg
                .register(Box::new(gauge.clone()))
                .expect("failed to register Gauge");
            Gauge { gauge }
        }
    }

    #[derive(Clone)]
    pub struct Gauge {
        gauge: prometheus::GaugeVec,
    }

    impl Gauge {
        pub fn set(&self, label: &str, value: f64) {
            self.gauge.with_label_values(&[label]).set(value)
        }
    }

    #[derive(Clone)]
    pub struct Histogram {
        hist: HistogramVec,
    }

    impl Histogram {
        pub fn observe(&self, label: &str, value: f64) {
            self.hist.with_label_values(&[label]).observe(value);
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
pub use metrics_impl::{Gauge, Histogram, Metrics};

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

        pub fn histogram_vec(
            &self,
            _name: &str,
            _desc: &str,
            _label: &str,
            _buckets: Vec<f64>,
        ) -> Histogram {
            Histogram {}
        }

        pub fn gauge(&self, _name: &str, _desc: &str, _label: &str) -> Gauge {
            Gauge {}
        }
    }

    #[derive(Clone)]
    pub struct Gauge {}

    impl Gauge {
        pub fn set(&self, _label: &str, _value: f64) {}
    }

    #[derive(Clone)]
    pub struct Histogram {}

    impl Histogram {
        pub fn observe(&self, _label: &str, _value: f64) {}

        pub fn observe_duration<F, T>(&self, _label: &str, func: F) -> T
        where
            F: FnOnce() -> T,
        {
            func()
        }
    }
}

#[cfg(not(feature = "metrics"))]
pub use metrics_fake::{Gauge, Histogram, Metrics};

pub(crate) fn default_duration_buckets() -> Vec<f64> {
    vec![
        1e-6, 2e-6, 5e-6, 1e-5, 2e-5, 5e-5, 1e-4, 2e-4, 5e-4, 1e-3, 2e-3, 5e-3, 1e-2, 2e-2, 5e-2,
        1e-1, 2e-1, 5e-1, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0,
    ]
}

pub(crate) fn default_size_buckets() -> Vec<f64> {
    vec![
        1.0, 2.0, 5.0, 1e1, 2e1, 5e1, 1e2, 2e2, 5e2, 1e3, 2e3, 5e3, 1e4, 2e4, 5e4, 1e5, 2e5, 5e5,
        1e6, 2e6, 5e6, 1e7,
    ]
}
