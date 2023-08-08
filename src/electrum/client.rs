use std::collections::HashMap;
use std::convert::TryFrom;

use bitcoin::hashes::{sha256d, Hash};
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
    fn try_from(features: ServerFeaturesRes) -> Result<Self, Self::Error> {
        let genesis_hash = {
            let mut genesis_hash = features.genesis_hash;
            genesis_hash.reverse();
            BlockHash::from_raw_hash(sha256d::Hash::from_byte_array(genesis_hash))
        };

        Ok(ServerFeatures {
            // electrum-client doesn't retain the hosts map data, but we already have it from the add_peer request
            hosts: HashMap::new(),
            genesis_hash,
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
