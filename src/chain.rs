#[cfg(not(feature = "liquid"))] // use regular Bitcoin data structures
pub use bitcoin::{
    blockdata::script, consensus::deserialize, util::address, Block, BlockHash, BlockHeader,
    OutPoint, Script, Transaction, TxIn, TxOut, Txid,
};

#[cfg(feature = "liquid")]
pub use {
    crate::elements::asset,
    elements::{
        address, confidential, encode::deserialize, script, Address, AssetId, Block, BlockHash,
        BlockHeader, OutPoint, Script, Transaction, TxIn, TxOut, Txid,
    },
};

use bitcoin::blockdata::constants::genesis_block;
pub use bitcoin::network::constants::Network as BNetwork;

#[cfg(not(feature = "liquid"))]
pub type Value = u64;
#[cfg(feature = "liquid")]
pub use confidential::Value;

#[derive(Debug, Copy, Clone, PartialEq, Hash, Serialize, Ord, PartialOrd, Eq)]
pub enum Network {
    #[cfg(not(feature = "liquid"))]
    Bitcoin,
    #[cfg(not(feature = "liquid"))]
    Testnet,
    #[cfg(not(feature = "liquid"))]
    Regtest,
    #[cfg(not(feature = "liquid"))]
    Signet,

    #[cfg(feature = "liquid")]
    Liquid,
    #[cfg(feature = "liquid")]
    LiquidTestnet,
    #[cfg(feature = "liquid")]
    LiquidRegtest,
}

#[cfg(feature = "liquid")]
pub const LIQUID_TESTNET_PARAMS: address::AddressParams = address::AddressParams {
    p2pkh_prefix: 36,
    p2sh_prefix: 19,
    blinded_prefix: 23,
    bech_hrp: "tex",
    blech_hrp: "tlq",
};

impl Network {
    #[cfg(not(feature = "liquid"))]
    pub fn magic(self) -> u32 {
        BNetwork::from(self).magic()
    }

    #[cfg(feature = "liquid")]
    pub fn magic(self) -> u32 {
        match self {
            Network::Liquid | Network::LiquidRegtest => 0xDAB5_BFFA,
            Network::LiquidTestnet => 0x62DD_0E41,
        }
    }

    pub fn is_regtest(self) -> bool {
        match self {
            #[cfg(not(feature = "liquid"))]
            Network::Regtest => true,
            #[cfg(feature = "liquid")]
            Network::LiquidRegtest => true,
            _ => false,
        }
    }

    #[cfg(feature = "liquid")]
    pub fn address_params(self) -> &'static address::AddressParams {
        // Liquid regtest uses elements's address params
        match self {
            Network::Liquid => &address::AddressParams::LIQUID,
            Network::LiquidRegtest => &address::AddressParams::ELEMENTS,
            Network::LiquidTestnet => &LIQUID_TESTNET_PARAMS,
        }
    }

    #[cfg(feature = "liquid")]
    pub fn native_asset(self) -> &'static AssetId {
        match self {
            Network::Liquid => &*asset::NATIVE_ASSET_ID,
            Network::LiquidTestnet => &*asset::NATIVE_ASSET_ID_TESTNET,
            Network::LiquidRegtest => &*asset::NATIVE_ASSET_ID_REGTEST,
        }
    }

    #[cfg(feature = "liquid")]
    pub fn pegged_asset(self) -> Option<&'static AssetId> {
        match self {
            Network::Liquid => Some(&*asset::NATIVE_ASSET_ID),
            Network::LiquidTestnet | Network::LiquidRegtest => None,
        }
    }

    pub fn names() -> Vec<String> {
        #[cfg(not(feature = "liquid"))]
        return vec![
            "mainnet".to_string(),
            "testnet".to_string(),
            "regtest".to_string(),
            "signet".to_string(),
        ];

        #[cfg(feature = "liquid")]
        return vec![
            "liquid".to_string(),
            "liquidtestnet".to_string(),
            "liquidregtest".to_string(),
        ];
    }
}

pub fn genesis_hash(network: Network) -> BlockHash {
    #[cfg(not(feature = "liquid"))]
    return bitcoin_genesis_hash(network.into());
    #[cfg(feature = "liquid")]
    return liquid_genesis_hash(network);
}

pub fn bitcoin_genesis_hash(network: BNetwork) -> bitcoin::BlockHash {
    lazy_static! {
        static ref BITCOIN_GENESIS: bitcoin::BlockHash =
            genesis_block(BNetwork::Bitcoin).block_hash();
        static ref TESTNET_GENESIS: bitcoin::BlockHash =
            genesis_block(BNetwork::Testnet).block_hash();
        static ref REGTEST_GENESIS: bitcoin::BlockHash =
            genesis_block(BNetwork::Regtest).block_hash();
        static ref SIGNET_GENESIS: bitcoin::BlockHash =
            genesis_block(BNetwork::Signet).block_hash();
    }
    match network {
        BNetwork::Bitcoin => *BITCOIN_GENESIS,
        BNetwork::Testnet => *TESTNET_GENESIS,
        BNetwork::Regtest => *REGTEST_GENESIS,
        BNetwork::Signet => *SIGNET_GENESIS,
    }
}

#[cfg(feature = "liquid")]
pub fn liquid_genesis_hash(network: Network) -> elements::BlockHash {
    lazy_static! {
        static ref LIQUID_GENESIS: BlockHash =
            "1466275836220db2944ca059a3a10ef6fd2ea684b0688d2c379296888a206003"
                .parse()
                .unwrap();
    }

    match network {
        Network::Liquid => *LIQUID_GENESIS,
        // The genesis block for liquid regtest chains varies based on the chain configuration.
        // This instead uses an all zeroed-out hash, which doesn't matter in practice because its
        // only used for Electrum server discovery, which isn't active on regtest.
        _ => Default::default(),
    }
}

impl From<&str> for Network {
    fn from(network_name: &str) -> Self {
        match network_name {
            #[cfg(not(feature = "liquid"))]
            "mainnet" => Network::Bitcoin,
            #[cfg(not(feature = "liquid"))]
            "testnet" => Network::Testnet,
            #[cfg(not(feature = "liquid"))]
            "regtest" => Network::Regtest,
            #[cfg(not(feature = "liquid"))]
            "signet" => Network::Signet,

            #[cfg(feature = "liquid")]
            "liquid" => Network::Liquid,
            #[cfg(feature = "liquid")]
            "liquidtestnet" => Network::LiquidTestnet,
            #[cfg(feature = "liquid")]
            "liquidregtest" => Network::LiquidRegtest,

            _ => panic!("unsupported Bitcoin network: {:?}", network_name),
        }
    }
}

#[cfg(not(feature = "liquid"))]
impl From<Network> for BNetwork {
    fn from(network: Network) -> Self {
        match network {
            Network::Bitcoin => BNetwork::Bitcoin,
            Network::Testnet => BNetwork::Testnet,
            Network::Regtest => BNetwork::Regtest,
            Network::Signet => BNetwork::Signet,
        }
    }
}

#[cfg(not(feature = "liquid"))]
impl From<BNetwork> for Network {
    fn from(network: BNetwork) -> Self {
        match network {
            BNetwork::Bitcoin => Network::Bitcoin,
            BNetwork::Testnet => Network::Testnet,
            BNetwork::Regtest => Network::Regtest,
            BNetwork::Signet => Network::Signet,
        }
    }
}
