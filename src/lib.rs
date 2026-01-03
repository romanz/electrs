#[macro_use]
extern crate anyhow;

#[macro_use]
extern crate log;

#[macro_use]
extern crate serde_derive;

mod config;
mod daemon;
mod electrum;
mod mempool;
mod merkle;
mod metrics;
mod server;
mod signals;
mod status;
mod thread;
mod tracker;
mod types;

pub use server::run;

use bindex::bitcoin;
