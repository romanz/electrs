#![recursion_limit = "1024"]

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

#[cfg(not(any(feature = "otlp-tracing", feature = "no-otlp-tracing")))]
compile_error!("Must enable one of the 'otlp-tracing' or 'no-otlp-tracing' features");

#[cfg(all(feature = "otlp-tracing", feature = "no-otlp-tracing"))]
compile_error!("Cannot enable both the 'otlp-tracing' and 'no-otlp-tracing' (default) features");

#[cfg(feature = "otlp-tracing")]
pub mod otlp_trace;
