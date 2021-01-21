use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::net::ToSocketAddrs;

use bitcoin::hashes::Hash;
use electrum_client::client::{
    Client as RClient, ElectrumPlaintextStream, ElectrumProxyStream, ElectrumSslStream,
};
pub use electrum_client::types::ServerFeaturesRes;
pub use electrum_client::Error as ElectrumError;

use crate::chain::BlockHash;
use crate::electrum::ServerFeatures;
use crate::errors::{Error, ResultExt};

pub enum Client {
    Tcp(RClient<ElectrumPlaintextStream>),
    Ssl(RClient<ElectrumSslStream>),
    ProxyTcp(RClient<ElectrumProxyStream>),
    // proxy+ssl on the same connection appears to be unsupported by the electrum_client crate
}

// impl<S: Read + Write> Client<S> {

impl Client {
    pub fn new<A: ToSocketAddrs>(socket_addr: A) -> Result<Self, ElectrumError> {
        Ok(Client::Tcp(RClient::new(socket_addr)?))
    }

    pub fn new_ssl(domain_addr: (&str, u16)) -> Result<Self, ElectrumError> {
        // SSL certificates are not validated
        Ok(Client::Ssl(RClient::new_ssl(domain_addr, false)?))
        // XXX this should ideally use the previously resolved IP address instead of the hostname, which
        // shuold be possible because we don't validate, but it appears like rustls does not support this.
    }

    pub fn new_proxy<A: ToSocketAddrs>(
        target_addr: (&str, u16),
        proxy_addr: A,
    ) -> Result<Self, ElectrumError> {
        Ok(Client::ProxyTcp(RClient::new_proxy(
            target_addr,
            proxy_addr,
        )?))
    }

    pub fn server_features(&mut self) -> Result<ServerFeatures, Error> {
        match self {
            Client::Tcp(c) => c.server_features(),
            Client::Ssl(c) => c.server_features(),
            Client::ProxyTcp(c) => c.server_features(),
        }?
        .try_into()
    }

    pub fn server_add_peer(&mut self, features: &ServerFeatures) -> Result<bool, ElectrumError> {
        match self {
            Client::Tcp(c) => c.server_add_peer(features),
            Client::Ssl(c) => c.server_add_peer(features),
            Client::ProxyTcp(c) => c.server_add_peer(features),
        }
    }
}

// Convert from electrum-client's server features struct to ours. We're using a different struct because
// the electrum-client's one doesn't support the "hosts" key.
impl TryFrom<ServerFeaturesRes> for ServerFeatures {
    type Error = Error;
    fn try_from(mut features: ServerFeaturesRes) -> Result<Self, Self::Error> {
        features.genesis_hash.reverse();

        Ok(ServerFeatures {
            // electrum-client doesn't retain the hosts map data, but we already have it from the add_peer request
            hosts: HashMap::new(),
            genesis_hash: BlockHash::from_inner(features.genesis_hash),
            server_version: features.server_version,
            protocol_min: features
                .protocol_min
                .parse()
                .chain_err(|| "invalid protocol_min")?,
            protocol_max: features
                .protocol_max
                .parse()
                .chain_err(|| "invalid protocol_max")?,
            pruning: features.pruning.map(|pruning| pruning as usize),
            hash_function: features
                .hash_function
                .chain_err(|| "missing hash_function")?,
        })
    }
}
