use bitcoin::blockdata::script::{Instruction::PushBytes, Script};
#[cfg(not(feature = "liquid"))]
use bitcoin::network::constants::Network as BNetwork;
#[cfg(not(feature = "liquid"))]
use bitcoin_bech32::constants::Network as B32Network;
use bitcoin_bech32::{self, u5};
use bitcoin_hashes::{hash160::Hash as Hash160, Hash};

use crate::chain::{address, Network};
use crate::chain::{TxIn, TxOut};

pub struct InnerScripts {
    pub redeem_script: Option<Script>,
    pub witness_script: Option<Script>,
}

// @XXX we can't use any of the Address:p2{...}h utility methods, since they expect the pre-image data, which we don't have.
// we must instead create the Payload manually, which results in code duplication with the p2{...}h methods, especially for witness programs.
// ideally, this should be implemented as part of the rust-bitcoin lib.
pub fn script_to_address(script: &Script, network: &Network) -> Option<String> {
    let payload = if script.is_p2pkh() {
        address::Payload::PubkeyHash(Hash160::from_slice(&script[3..23]).ok()?)
    } else if script.is_p2sh() {
        address::Payload::ScriptHash(Hash160::from_slice(&script[2..22]).ok()?)
    } else if script.is_v0_p2wpkh() || script.is_v0_p2wsh() {
        let version = u5::try_from_u8(0).expect("0<32");
        let program = if script.is_v0_p2wpkh() {
            script[2..22].to_vec()
        } else {
            script[2..34].to_vec()
        };

        #[cfg(not(feature = "liquid"))]
        {
            address::Payload::WitnessProgram(
                bitcoin_bech32::WitnessProgram::new(version, program, B32Network::from(network))
                    .unwrap(),
            )
        }
        #[cfg(feature = "liquid")]
        {
            address::Payload::WitnessProgram { version, program }
        }
    } else {
        return None;
    };

    Some(
        address::Address {
            payload,

            #[cfg(not(feature = "liquid"))]
            network: BNetwork::from(network),

            #[cfg(feature = "liquid")]
            params: network.address_params(),
            #[cfg(feature = "liquid")]
            blinding_pubkey: None,
        }
        .to_string(),
    )
}

pub fn get_script_asm(script: &Script) -> String {
    let asm = format!("{:?}", script);
    (&asm[7..asm.len() - 1]).to_string()
}

// Returns the witnessScript in the case of p2wsh, or the redeemScript in the case of p2sh.
pub fn get_innerscripts(txin: &TxIn, prevout: &TxOut) -> InnerScripts {
    // Wrapped redeemScript for P2SH spends
    let redeem_script = if prevout.script_pubkey.is_p2sh() {
        if let Some(PushBytes(redeemscript)) = txin.script_sig.iter(true).last() {
            Some(Script::from(redeemscript.to_vec()))
        } else {
            None
        }
    } else {
        None
    };

    // Wrapped witnessScript for P2WSH or P2SH-P2WSH spends
    #[cfg(not(feature = "liquid"))]
    let witness_script = if prevout.script_pubkey.is_v0_p2wsh()
        || redeem_script.as_ref().map_or(false, |s| s.is_v0_p2wsh())
    {
        txin.witness.iter().last().cloned().map(Script::from)
    } else {
        None
    };

    // TODO: witness for elements
    #[cfg(feature = "liquid")]
    let witness_script = None;

    InnerScripts {
        redeem_script,
        witness_script,
    }
}
