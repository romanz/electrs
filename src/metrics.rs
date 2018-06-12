use prometheus::{self, Encoder};
use std::io;
use std::net::SocketAddr;
use tiny_http;

pub use prometheus::{IntCounter as Counter, Opts as MetricOpts};

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
        let cnt = Counter::with_opts(opts).unwrap();
        self.reg.register(Box::new(cnt.clone())).unwrap();
        cnt
    }

    pub fn serve(&self) {
        let server = tiny_http::Server::http(self.addr).unwrap();
        loop {
            if let Err(e) = self.handle(server.recv()) {
                error!("http error: {}", e);
            }
        }
    }
    fn handle(&self, req: io::Result<tiny_http::Request>) -> io::Result<()> {
        let request = req?;
        let mut buffer = vec![];
        prometheus::TextEncoder::new()
            .encode(&self.reg.gather(), &mut buffer)
            .unwrap();
        let response = tiny_http::Response::from_data(buffer);
        request.respond(response)
    }
}
