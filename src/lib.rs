#[macro_use]
extern crate anyhow;

#[macro_use]
extern crate log;

#[macro_use]
extern crate serde_derive;

extern crate configure_me;

mod cache;
mod chain;
mod config;
mod daemon;
#[cfg(feature = "heed")]
mod db_heed;
#[cfg(feature = "lmdb-rkv")]
mod db_lmdb_rkv;
#[cfg(feature = "redb")]
mod db_redb;
#[cfg(feature = "rocksdb")]
mod db_rocksdb;
#[cfg(feature = "sled")]
mod db_sled;
mod db_store;
mod electrum;
mod index;
mod mempool;
mod merkle;
mod metrics;
mod p2p;
mod server;
mod signals;
mod status;
mod thread;
mod tracker;
mod types;

pub use server::run;
