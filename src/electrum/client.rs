use std::collections::HashMap;
use std::convert::TryFrom;

use bitcoin::hashes::Hash;
pub use electrum_client::client::Client;
pub use electrum_client::Error as ElectrumError;
pub use electrum_client::ServerFeaturesRes;

use crate::chain::BlockHash;
use crate::electrum::ServerFeatures;
use crate::errors::{Error, ResultExt};

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
