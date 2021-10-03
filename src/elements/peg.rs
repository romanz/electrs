use bitcoin::hashes::hex::ToHex;
use elements::{confidential::Asset, PeginData, PegoutData, TxIn, TxOut};

use crate::chain::{bitcoin_genesis_hash, BNetwork, Network};
use crate::util::{FullHash, ScriptToAsm};

pub fn get_pegin_data(txout: &TxIn, network: Network) -> Option<PeginData> {
    let pegged_asset_id = network.pegged_asset()?;
    txout
        .pegin_data()
        .filter(|pegin| pegin.asset == Asset::Explicit(*pegged_asset_id))
}

pub fn get_pegout_data(
    txout: &TxOut,
    network: Network,
    parent_network: BNetwork,
) -> Option<PegoutData> {
    let pegged_asset_id = network.pegged_asset()?;
    txout.pegout_data().filter(|pegout| {
        pegout.asset == Asset::Explicit(*pegged_asset_id)
            && pegout.genesis_hash == bitcoin_genesis_hash(parent_network)
    })
}

// API representation of pegout data assocaited with an output
#[derive(Serialize, Deserialize, Clone)]
pub struct PegoutValue {
    pub genesis_hash: String,
    pub scriptpubkey: bitcoin::Script,
    pub scriptpubkey_asm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scriptpubkey_address: Option<String>,
}

impl PegoutValue {
    pub fn from_txout(txout: &TxOut, network: Network, parent_network: BNetwork) -> Option<Self> {
        let pegoutdata = get_pegout_data(txout, network, parent_network)?;

        // pending https://github.com/ElementsProject/rust-elements/pull/69 is merged
        let scriptpubkey = bitcoin::Script::from(pegoutdata.script_pubkey.into_bytes());
        let address = bitcoin::Address::from_script(&scriptpubkey, parent_network);

        Some(PegoutValue {
            genesis_hash: pegoutdata.genesis_hash.to_hex(),
            scriptpubkey_asm: scriptpubkey.to_asm(),
            scriptpubkey_address: address.map(|s| s.to_string()),
            scriptpubkey,
        })
    }
}

// Inner type for the indexer TxHistoryInfo::Pegin variant
#[derive(Serialize, Deserialize, Debug)]
pub struct PeginInfo {
    pub txid: FullHash,
    pub vin: u16,
    pub value: u64,
}

// Inner type for the indexer TxHistoryInfo::Pegout variant
#[derive(Serialize, Deserialize, Debug)]
pub struct PegoutInfo {
    pub txid: FullHash,
    pub vout: u16,
    pub value: u64,
}
