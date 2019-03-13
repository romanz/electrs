use crate::chain::Network;
use crate::util::{Address, Payload};
use bitcoin::Script;
use bitcoin_bech32::constants::Network as B32Network;
use bitcoin_bech32::{u5, WitnessProgram};
use bitcoin_hashes::{hash160::Hash as Hash160, Hash};

// @XXX we can't use any of the Address:p2{...}h utility methods, since they expect the pre-image data, which we don't have.
// we must instead create the Payload manually, which results in code duplication with the p2{...}h methods, especially for witness programs.
// ideally, this should be implemented as part of the rust-bitcoin lib.
pub fn script_to_address(script: &Script, network: &Network) -> Option<String> {
    let payload = if script.is_p2pkh() {
        Some(Payload::PubkeyHash(
            Hash160::from_slice(&script[3..23]).ok()?,
        ))
    } else if script.is_p2sh() {
        Some(Payload::ScriptHash(
            Hash160::from_slice(&script[2..22]).ok()?,
        ))
    } else if script.is_v0_p2wpkh() {
        Some(Payload::WitnessProgram(
            WitnessProgram::new(
                u5::try_from_u8(0).expect("0<32"),
                script[2..22].to_vec(),
                B32Network::from(network),
            )
            .unwrap(),
        ))
    } else if script.is_v0_p2wsh() {
        Some(Payload::WitnessProgram(
            WitnessProgram::new(
                u5::try_from_u8(0).expect("0<32"),
                script[2..34].to_vec(),
                B32Network::from(network),
            )
            .unwrap(),
        ))
    } else {
        None
    };

    Some(
        Address {
            payload: payload?,
            network: *network,
        }
        .to_string(),
    )
}

pub fn get_script_asm(script: &Script) -> String {
    let asm = format!("{:?}", script);
    (&asm[7..asm.len() - 1]).to_string()
}
