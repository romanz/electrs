#[macro_use]
extern crate anyhow;

#[macro_use]
extern crate log;

#[macro_use]
extern crate serde_derive;

extern crate configure_me;

// export specific versions of rust-bitcoin crates
pub use bitcoin;
pub use bitcoincore_rpc;

mod config;
mod daemon;
mod db;
mod index;
mod map;
mod metrics;
mod types;
mod undo;

pub use {
    config::Config,
    daemon::Daemon,
    db::DBStore,
    index::Index,
    metrics::{Gauge, GaugeVec, Histogram, Metrics},
    types::{Confirmed, ScriptHash},
};
