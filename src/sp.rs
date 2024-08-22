use bitcoin::{key::Secp256k1, secp256k1::{silentpayments::SilentpaymentsPublicData, PublicKey, XOnlyPublicKey}, Txid};
use bitcoin_slices::bitcoin_hashes::{hash160, Hash};

use anyhow::Error;

// ** Putting all the pubkey extraction logic in the test utils for now. **
// NUMS_H (defined in BIP340)
const NUMS_H: [u8; 32] = [
    0x50, 0x92, 0x9b, 0x74, 0xc1, 0xa0, 0x49, 0x54, 0xb7, 0x8b, 0x4b, 0x60, 0x35, 0xe9, 0x7a, 0x5e,
    0x07, 0x8a, 0x5a, 0x0f, 0x28, 0xec, 0x96, 0xd5, 0x47, 0xbf, 0xee, 0x9a, 0xce, 0x80, 0x3a, 0xc0,
];

// Define OP_CODES used in script template matching for readability
const OP_1: u8 = 0x51;
const OP_0: u8 = 0x00;
const OP_PUSHBYTES_20: u8 = 0x14;
const OP_PUSHBYTES_32: u8 = 0x20;
const OP_HASH160: u8 = 0xA9;
const OP_EQUAL: u8 = 0x87;
const OP_DUP: u8 = 0x76;
const OP_EQUALVERIFY: u8 = 0x88;
const OP_CHECKSIG: u8 = 0xAC;

// Only compressed pubkeys are supported for silent payments
const COMPRESSED_PUBKEY_SIZE: usize = 33;

pub struct VinData {
    pub script_sig: Vec<u8>,
    pub txinwitness: Vec<Vec<u8>>,
    pub script_pub_key: Vec<u8>,
}

// script templates for inputs allowed in BIP352 shared secret derivation
pub fn is_p2tr(spk: &[u8]) -> bool {
    matches!(spk, [OP_1, OP_PUSHBYTES_32, ..] if spk.len() == 34)
}

fn is_p2wpkh(spk: &[u8]) -> bool {
    matches!(spk, [OP_0, OP_PUSHBYTES_20, ..] if spk.len() == 22)
}

fn is_p2sh(spk: &[u8]) -> bool {
    matches!(spk, [OP_HASH160, OP_PUSHBYTES_20, .., OP_EQUAL] if spk.len() == 23)
}

fn is_p2pkh(spk: &[u8]) -> bool {
    matches!(spk, [OP_DUP, OP_HASH160, OP_PUSHBYTES_20, .., OP_EQUALVERIFY, OP_CHECKSIG] if spk.len() == 25)
}

pub enum PubKeyFromInput {
    XOnlyPublicKey(XOnlyPublicKey),
    PublicKey(PublicKey),
}

pub fn get_pubkey_from_input(vin: &VinData) -> Result<Option<PubKeyFromInput>, Error> {
    if is_p2pkh(&vin.script_pub_key) {
        match (&vin.txinwitness.is_empty(), &vin.script_sig.is_empty()) {
            (true, false) => {
                let spk_hash = &vin.script_pub_key[3..23];
                for i in (COMPRESSED_PUBKEY_SIZE..=vin.script_sig.len()).rev() {
                    if let Some(pubkey_bytes) = &vin.script_sig.get(i - COMPRESSED_PUBKEY_SIZE..i) {
                        let pubkey_hash = hash160::Hash::hash(pubkey_bytes);
                        if pubkey_hash.to_byte_array() == spk_hash {
                            let pubkey = PublicKey::from_slice(pubkey_bytes)?;
                            let result = PubKeyFromInput::PublicKey(pubkey);
                            return Ok(Some(result));
                        }
                    } else {
                        return Ok(None);
                    }
                }
            }
            (_, true) => return Err(Error::msg("Empty script_sig for spending a p2pkh")),
            (false, _) => return Err(Error::msg("non empty witness for spending a p2pkh")),
        }
    } else if is_p2sh(&vin.script_pub_key) {
        match (&vin.txinwitness.is_empty(), &vin.script_sig.is_empty()) {
            (false, false) => {
                let redeem_script = &vin.script_sig[1..];
                if is_p2wpkh(redeem_script) {
                    if let Some(value) = vin.txinwitness.last() {
                        if let Ok(pubkey) = PublicKey::from_slice(value) {
                            let result = PubKeyFromInput::PublicKey(pubkey);
                            return Ok(Some(result));
                        } else {
                            return Ok(None);
                        }
                    }
                }
            }
            (_, true) => {
                return Err(Error::msg(
                    "Empty script_sig for spending a p2sh".to_owned(),
                ))
            }
            (true, false) => {
                return Ok(None);
            }
        }
    } else if is_p2wpkh(&vin.script_pub_key) {
        match (&vin.txinwitness.is_empty(), &vin.script_sig.is_empty()) {
            (false, true) => {
                if let Some(value) = vin.txinwitness.last() {
                    if let Ok(pubkey) = PublicKey::from_slice(value) {
                        let result = PubKeyFromInput::PublicKey(pubkey);
                        return Ok(Some(result));
                    } else {
                        return Ok(None);
                    }
                } else {
                    return Err(Error::msg("Empty witness".to_owned()));
                }
            }
            (_, false) => {
                return Err(Error::msg(
                    "Non empty script sig for spending a segwit output".to_owned(),
                ))
            }
            (true, _) => {
                return Err(Error::msg(
                    "Empty witness for spending a segwit output".to_owned(),
                ))
            }
        }
    } else if is_p2tr(&vin.script_pub_key) {
        match (&vin.txinwitness.is_empty(), &vin.script_sig.is_empty()) {
            (false, true) => {
                // check for the optional annex
                let annex = match vin.txinwitness.last().and_then(|value| value.get(0)) {
                    Some(&0x50) => 1,
                    Some(_) => 0,
                    None => return Err(Error::msg("Empty or invalid witness".to_owned())),
                };

                // Check for script path
                let stack_size = vin.txinwitness.len();
                if stack_size > annex && vin.txinwitness[stack_size - annex - 1][1..33] == NUMS_H {
                    return Ok(None);
                }

                // Return the pubkey from the script pubkey
                return XOnlyPublicKey::from_slice(&vin.script_pub_key[2..34])
                    .map_err(|e| Error::new(e))
                    .map(|x_only_public_key| {
                        let result = PubKeyFromInput::XOnlyPublicKey(x_only_public_key);
                        Some(result)
                    });
            }
            (_, false) => {
                return Err(Error::msg(
                    "Non empty script sig for spending a segwit output".to_owned(),
                ))
            }
            (true, _) => {
                return Err(Error::msg(
                    "Empty witness for spending a segwit output".to_owned(),
                ))
            }
        }
    }
    return Ok(None);
}


pub fn get_smallest_outpoint(outpoints_data: &[(Txid, u32)],) -> Result<[u8; 36], Error> {
    if outpoints_data.is_empty() {
        return Err(Error::msg("No outpoints provided"));
    }

    let mut outpoints: Vec<[u8; 36]> = Vec::with_capacity(outpoints_data.len());

    for (txid, vout) in outpoints_data {
        let bytes = bitcoin::consensus::encode::serialize(&txid);

        if bytes.len() != 32 {
            return Err(Error::msg(format!(
                "Invalid outpoint hex representation: {}",
                txid
            )));
        }

        let mut buffer = [0u8; 36];

        buffer[..32].copy_from_slice(&bytes);
        buffer[32..].copy_from_slice(&vout.to_le_bytes());
        outpoints.push(buffer);
    }

    // sort outpoints
    outpoints.sort_unstable();

    if let Some(smallest_outpoint) = outpoints.first() {
        Ok(smallest_outpoint.clone())
    } else {
        // This should never happen
        Err(Error::msg("Unexpected empty outpoints vector"))
    }
}

pub fn recipient_calculate_tweak_data(
    input_xpub_keys: Option<&[XOnlyPublicKey]>,
    input_pub_keys: Option<&[PublicKey]>,
    outpoints_data: &[(Txid, u32)],
) -> Result<[u8; 33], Error> {
    let smallest_outpoint = get_smallest_outpoint(outpoints_data).unwrap();
    let secp = Secp256k1::new();
    let public_data = SilentpaymentsPublicData::create(
        &secp,
        &smallest_outpoint,
        input_xpub_keys,
        input_pub_keys
    ).unwrap();
    let light_client_data33 = public_data.serialize(&secp).unwrap();
    return Ok(light_client_data33);
}
