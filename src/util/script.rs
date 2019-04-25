#[cfg(not(feature = "liquid"))]
use bitcoin::network::constants::Network as BNetwork;
use bitcoin::Script;
#[cfg(not(feature = "liquid"))]
use bitcoin_bech32::constants::Network as B32Network;
use bitcoin_bech32::{self, u5};
use bitcoin_hashes::{hash160::Hash as Hash160, Hash};

use crate::chain::{address, Network};

// @XXX we can't use any of the Address:p2{...}h utility methods, since they expect the pre-image data, which we don't have.
// we must instead create the Payload manually, which results in code duplication with the p2{...}h methods, especially for witness programs.
// ideally, this should be implemented as part of the rust-bitcoin lib.
#[allow(unused_variables)] // `network` is unused in liquid mode
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
