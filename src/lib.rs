#![recursion_limit = "1024"]

// See https://github.com/romanz/electrs/issues/193 & https://github.com/rust-rocksdb/rust-rocksdb/issues/327
#[cfg(not(feature = "oldcpu"))]
extern crate rocksdb;
#[cfg(feature = "oldcpu")]
extern crate rocksdb_oldcpu as rocksdb;

#[macro_use]
extern crate clap;
#[macro_use]
extern crate arrayref;
#[macro_use]
extern crate error_chain;
#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate serde_json;

#[macro_use]
extern crate lazy_static;

pub mod chain;
pub mod config;
pub mod daemon;
pub mod electrum;
pub mod errors;
pub mod metrics;
pub mod new_index;
pub mod rest;
pub mod signal;
pub mod util;

#[cfg(feature = "liquid")]
pub mod elements;
