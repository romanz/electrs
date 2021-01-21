mod server;
pub use server::RPC;

#[cfg(feature = "electrum-discovery")]
mod client;
#[cfg(feature = "electrum-discovery")]
mod discovery;
#[cfg(feature = "electrum-discovery")]
pub use {client::Client, discovery::DiscoveryManager};

use std::cmp::Ordering;
use std::collections::HashMap;
use std::str::FromStr;

use serde::{de, Deserialize, Deserializer, Serialize};

use crate::chain::BlockHash;
use crate::errors::ResultExt;
use crate::util::BlockId;

pub fn get_electrum_height(blockid: Option<BlockId>, has_unconfirmed_parents: bool) -> isize {
    match (blockid, has_unconfirmed_parents) {
        (Some(blockid), _) => blockid.height as isize,
        (None, false) => 0,
        (None, true) => -1,
    }
}

pub type Port = u16;
pub type Hostname = String;

pub type ServerHosts = HashMap<Hostname, ServerPorts>;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ServerFeatures {
    pub hosts: ServerHosts,
    pub genesis_hash: BlockHash,
    pub server_version: String,
    pub protocol_min: ProtocolVersion,
    pub protocol_max: ProtocolVersion,
    pub pruning: Option<usize>,
    pub hash_function: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ServerPorts {
    tcp_port: Option<Port>,
    ssl_port: Option<Port>,
}

#[derive(Eq, PartialEq, Debug, Clone, Default)]
pub struct ProtocolVersion {
    major: usize,
    minor: usize,
}

impl ProtocolVersion {
    pub const fn new(major: usize, minor: usize) -> Self {
        Self { major, minor }
    }
}

impl Ord for ProtocolVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
    }
}

impl PartialOrd for ProtocolVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl FromStr for ProtocolVersion {
    type Err = crate::errors::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut iter = s.split('.');
        Ok(Self {
            major: iter
                .next()
                .chain_err(|| "missing major")?
                .parse()
                .chain_err(|| "invalid major")?,
            minor: iter
                .next()
                .chain_err(|| "missing minor")?
                .parse()
                .chain_err(|| "invalid minor")?,
        })
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl Serialize for ProtocolVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(&self)
    }
}

impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        FromStr::from_str(&s).map_err(de::Error::custom)
    }
}
