#[cfg(not(feature = "liquid"))] // use regular Bitcoin data structures
pub use bitcoin::{util::address, Block, BlockHeader, OutPoint, Transaction, TxIn, TxOut};

#[cfg(feature = "liquid")]
pub use {
    crate::elements::asset,
    elements::{
        address, confidential, Address, AssetId, Block, BlockHeader, OutPoint, Transaction, TxIn,
        TxOut,
    },
};

use bitcoin::blockdata::constants::genesis_block;
use bitcoin::network::constants::Network as BNetwork;
use bitcoin::BlockHash;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[cfg(not(feature = "liquid"))]
pub type Value = u64;
#[cfg(feature = "liquid")]
pub use confidential::Value;

lazy_static! {
    static ref CACHED_GENESIS: Arc<RwLock<HashMap<Network, BlockHash>>> =
        Arc::new(RwLock::new(HashMap::new()));
}

#[derive(Debug, Copy, Clone, PartialEq, Hash, Serialize, Ord, PartialOrd, Eq)]
pub enum Network {
    Bitcoin,
    Testnet,
    Regtest,

    #[cfg(feature = "liquid")]
    Liquid,
    #[cfg(feature = "liquid")]
    LiquidRegtest,
}

impl Network {
    pub fn genesis_hash(self) -> BlockHash {
        if let Some(block_hash) = CACHED_GENESIS.read().unwrap().get(&self) {
            return *block_hash;
        }

        let block_hash = genesis_block(BNetwork::from(self)).block_hash();
        CACHED_GENESIS.write().unwrap().insert(self, block_hash);
        block_hash
    }

    pub fn magic(self) -> u32 {
        match self {
            Network::Bitcoin => 0xD9B4_BEF9,
            Network::Testnet => 0x0709_110B,
            Network::Regtest => 0xDAB5_BFFA,

            #[cfg(feature = "liquid")]
            Network::Liquid => 0xDAB5_BFFA,
            #[cfg(feature = "liquid")]
            Network::LiquidRegtest => 0xDAB5_BFFA,
        }
    }

    #[cfg(feature = "liquid")]
    pub fn address_params(self) -> &'static address::AddressParams {
        // Liquid regtest uses elements's address params
        match self {
            Network::Liquid => &address::AddressParams::LIQUID,
            Network::LiquidRegtest => &address::AddressParams::ELEMENTS,
            _ => panic!("the liquid-only address_params() called with non-liquid network"),
        }
    }

    #[cfg(feature = "liquid")]
    pub fn native_asset(self) -> &'static AssetId {
        match self {
            Network::Liquid => &*asset::NATIVE_ASSET_ID,
            // same for testnet and regtest
            Network::LiquidRegtest => &*asset::NATIVE_ASSET_ID_TESTNET,
            _ => panic!("the liquid-only native_asset_id() called with non-liquid network"),
        }
    }

    pub fn names() -> Vec<String> {
        #[cfg(not(feature = "liquid"))]
        return vec![
            "mainnet".to_string(),
            "testnet".to_string(),
            "regtest".to_string(),
        ];

        #[cfg(feature = "liquid")]
        return vec![
            "mainnet".to_string(),
            "testnet".to_string(),
            "regtest".to_string(),
            "liquid".to_string(),
            "liquidregtest".to_string(),
        ];
    }
}

impl From<&str> for Network {
    fn from(network_name: &str) -> Self {
        match network_name {
            "mainnet" => Network::Bitcoin,
            "testnet" => Network::Testnet,
            "regtest" => Network::Regtest,

            #[cfg(feature = "liquid")]
            "liquid" => Network::Liquid,
            #[cfg(feature = "liquid")]
            "liquidregtest" => Network::LiquidRegtest,

            _ => panic!("unsupported Bitcoin network: {:?}", network_name),
        }
    }
}

impl From<Network> for BNetwork {
    fn from(network: Network) -> Self {
        match network {
            Network::Bitcoin => BNetwork::Bitcoin,
            Network::Testnet => BNetwork::Testnet,
            Network::Regtest => BNetwork::Regtest,

            #[cfg(feature = "liquid")]
            Network::Liquid => BNetwork::Bitcoin, // @FIXME
            #[cfg(feature = "liquid")]
            Network::LiquidRegtest => BNetwork::Regtest, // @FIXME
        }
    }
}

impl From<BNetwork> for Network {
    fn from(network: BNetwork) -> Self {
        match network {
            #[cfg(not(feature = "liquid"))]
            BNetwork::Bitcoin => Network::Bitcoin,
            #[cfg(not(feature = "liquid"))]
            BNetwork::Regtest => Network::Regtest,

            #[cfg(feature = "liquid")]
            BNetwork::Bitcoin => Network::Liquid, // @FIXME
            #[cfg(feature = "liquid")]
            BNetwork::Regtest => Network::LiquidRegtest, // @FIXME
            BNetwork::Testnet => Network::Testnet, // @FIXME
        }
    }
}
