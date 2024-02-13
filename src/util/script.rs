#[cfg(feature = "liquid")]
use crate::elements::ebcompact::*;
#[cfg(feature = "liquid")]
use elements::address as elements_address;

use crate::chain::{script, Network, Script, TxIn, TxOut};
use script::Instruction::PushBytes;

pub struct InnerScripts {
    pub redeem_script: Option<Script>,
    pub witness_script: Option<Script>,
}

pub trait ScriptToAsm: std::fmt::Debug {
    fn to_asm(&self) -> String {
        let asm = format!("{:?}", self);
        (&asm[7..asm.len() - 1]).to_string()
    }
}
impl ScriptToAsm for bitcoin::ScriptBuf {}
#[cfg(feature = "liquid")]
impl ScriptToAsm for elements::Script {}

pub trait ScriptToAddr {
    fn to_address_str(&self, network: Network) -> Option<String>;
}
#[cfg(not(feature = "liquid"))]
impl ScriptToAddr for bitcoin::Script {
    fn to_address_str(&self, network: Network) -> Option<String> {
        bitcoin::Address::from_script(self, network.into())
            .map(|s| s.to_string())
            .ok()
    }
}
#[cfg(feature = "liquid")]
impl ScriptToAddr for elements::Script {
    fn to_address_str(&self, network: Network) -> Option<String> {
        elements_address::Address::from_script(self, None, network.address_params())
            .map(|a| a.to_string())
    }
}

// Returns the witnessScript in the case of p2wsh, or the redeemScript in the case of p2sh.
pub fn get_innerscripts(txin: &TxIn, prevout: &TxOut) -> InnerScripts {
    // Wrapped redeemScript for P2SH spends
    let redeem_script = if prevout.script_pubkey.is_p2sh() {
        if let Some(Ok(PushBytes(redeemscript))) = txin.script_sig.instructions().last() {
            #[cfg(not(feature = "liquid"))] // rust-bitcoin has a PushBytes wrapper type
            let redeemscript = redeemscript.as_bytes();
            Some(Script::from(redeemscript.to_vec()))
        } else {
            None
        }
    } else {
        None
    };

    // Wrapped witnessScript for P2WSH or P2SH-P2WSH spends
    let witness_script = if prevout.script_pubkey.is_p2wsh()
        || redeem_script.as_ref().map_or(false, |s| s.is_p2wsh())
    {
        let witness = &txin.witness;
        #[cfg(feature = "liquid")]
        let witness = &witness.script_witness;

        // rust-bitcoin returns witness items as a [u8] slice, while rust-elements returns a Vec<u8>
        #[cfg(not(feature = "liquid"))]
        let wit_to_vec = Vec::from;
        #[cfg(feature = "liquid")]
        let wit_to_vec = Clone::clone;

        witness.iter().last().map(wit_to_vec).map(Script::from)
    } else {
        None
    };

    InnerScripts {
        redeem_script,
        witness_script,
    }
}
