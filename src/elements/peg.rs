use bitcoin::{hashes::hex::ToHex, Script};
use elements::{confidential::Asset, PeginData, PegoutData, TxIn, TxOut};

use crate::chain::Network;
use crate::util::{get_script_asm, script_to_address, FullHash};

pub fn get_pegin_data(txout: &TxIn, network: Network) -> Option<PeginData> {
    txout
        .pegin_data()
        .filter(|pegin| pegin.asset == Asset::Explicit(*network.native_asset()))
}

pub fn get_pegout_data(
    txout: &TxOut,
    network: Network,
    parent_network: Network,
) -> Option<PegoutData> {
    txout.pegout_data().filter(|pegout| {
        pegout.asset == Asset::Explicit(*network.native_asset())
            && pegout.genesis_hash == parent_network.genesis_hash()
    })
}

// API representation of pegout data assocaited with an output
#[derive(Serialize, Deserialize, Clone)]
pub struct PegoutValue {
    pub genesis_hash: String,
    pub scriptpubkey: Script,
    pub scriptpubkey_asm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scriptpubkey_address: Option<String>,
}

impl PegoutValue {
    pub fn parse(txout: &TxOut, network: Network, parent_network: Network) -> Option<Self> {
        let pegoutdata = get_pegout_data(txout, network, parent_network)?;

        Some(PegoutValue {
            genesis_hash: pegoutdata.genesis_hash.to_hex(),
            scriptpubkey_asm: get_script_asm(&pegoutdata.script_pubkey),
            scriptpubkey_address: script_to_address(&pegoutdata.script_pubkey, parent_network),
            scriptpubkey: pegoutdata.script_pubkey,
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
