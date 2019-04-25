#[cfg(not(feature = "liquid"))]
pub use bitcoin::util::address;
#[cfg(not(feature = "liquid"))] // use regular Bitcoin data structures
pub use bitcoin::{Block, BlockHeader, OutPoint, Transaction, TxIn, TxOut};

#[cfg(feature = "liquid")]
pub use elements::address;
#[cfg(feature = "liquid")]
pub use elements::{confidential, Address, Block, BlockHeader, OutPoint, Transaction, TxIn, TxOut};

use bitcoin::blockdata::constants::genesis_block;
use bitcoin::network::constants::Network as BNetwork;
use bitcoin::util::hash::BitcoinHash;
use bitcoin_bech32::constants::Network as B32Network;
use bitcoin_hashes::sha256d::Hash as Sha256dHash;

#[cfg(not(feature = "liquid"))]
pub type Value = u64;
#[cfg(feature = "liquid")]
pub use confidential::Value;

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
    pub fn genesis_hash(&self) -> Sha256dHash {
        let block = genesis_block(BNetwork::from(self));
        block.bitcoin_hash()
    }

    pub fn magic(&self) -> u32 {
        match self {
            Network::Bitcoin => 0xD9B4BEF9,
            Network::Testnet => 0x0709110B,
            Network::Regtest => 0xDAB5BFFA,

            #[cfg(feature = "liquid")]
            Network::Liquid => 0xDAB5BFFA,
            #[cfg(feature = "liquid")]
            Network::LiquidRegtest => 0xDAB5BFFA,
        }
    }

    #[cfg(feature = "liquid")]
    pub fn address_params(&self) -> &'static address::AddressParams {
        // Liquid regtest uses elements's address params
        match self {
            Network::Liquid => &address::AddressParams::LIQUID,
            Network::LiquidRegtest => &address::AddressParams::ELEMENTS,
            _ => panic!("the liquid-only address_params() called with non-liquid network"),
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

impl From<&Network> for BNetwork {
    fn from(network: &Network) -> Self {
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

impl From<&Network> for B32Network {
    fn from(network: &Network) -> Self {
        match network {
            Network::Bitcoin => B32Network::Bitcoin,
            Network::Testnet => B32Network::Testnet,
            Network::Regtest => B32Network::Regtest,
            #[cfg(feature = "liquid")]
            Network::Liquid => B32Network::Bitcoin, // @FIXME
            #[cfg(feature = "liquid")]
            Network::LiquidRegtest => B32Network::Regtest, // @FIXME
        }
    }
}

impl From<&BNetwork> for Network {
    fn from(network: &BNetwork) -> Self {
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
