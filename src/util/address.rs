// Rust Bitcoin Library
// Written in 2014 by
//     Andrew Poelstra <apoelstra@wpsoftware.net>
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the CC0 Public Domain Dedication
// along with this software.
// If not, see <http://creativecommons.org/publicdomain/zero/1.0/>.
//

//! Addresses
//!
//! Support for ordinary base58 Bitcoin addresses and private keys
//!
//! # Example: creating a new address from a randomly-generated key pair
//!
//! ```rust
//! extern crate rand;
//! extern crate secp256k1;
//! extern crate bitcoin;
//!
//! use bitcoin::network::constants::Network;
//! use bitcoin::util::address::Address;
//! use bitcoin::util::key;
//! use secp256k1::Secp256k1;
//! use rand::thread_rng;
//!
//! fn main() {
//!     // Generate random key pair
//!     let s = Secp256k1::new();
//!     let public_key = key::PublicKey {
//!         compressed: true,
//!         key: s.generate_keypair(&mut thread_rng()).1,
//!     };
//!
//!     // Generate pay-to-pubkey-hash address
//!     let address = Address::p2pkh(&public_key, Network::Bitcoin);
//! }
//! ```

use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use bitcoin_bech32::{self, u5, WitnessProgram};
use bitcoin_hashes::{hash160, Hash};

use crate::chain::Network;
use bitcoin::blockdata::opcodes;
use bitcoin::blockdata::script;
use bitcoin::consensus::encode;
use bitcoin::util::base58;
use bitcoin::util::key;

/// The method used to produce an address
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Payload {
    /// pay-to-pkhash address
    PubkeyHash(hash160::Hash),
    /// P2SH address
    ScriptHash(hash160::Hash),
    /// Segwit address
    WitnessProgram(WitnessProgram),
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// A Bitcoin address
pub struct Address {
    /// The type of the address
    pub payload: Payload,
    /// The network on which this address is usable
    pub network: Network,
}

impl Address {
    /// Creates a pay to (compressed) public key hash address from a public key
    /// This is the preferred non-witness type address
    #[inline]
    pub fn p2pkh(pk: &key::PublicKey, network: Network) -> Address {
        let mut hash_engine = hash160::Hash::engine();
        pk.write_into(&mut hash_engine);

        Address {
            network: network,
            payload: Payload::PubkeyHash(hash160::Hash::from_engine(hash_engine)),
        }
    }

    /// Creates a pay to script hash P2SH address from a script
    /// This address type was introduced with BIP16 and is the popular type to implement multi-sig these days.
    #[inline]
    pub fn p2sh(script: &script::Script, network: Network) -> Address {
        Address {
            network: network,
            payload: Payload::ScriptHash(hash160::Hash::hash(&script[..])),
        }
    }

    /// Create a witness pay to public key address from a public key
    /// This is the native segwit address type for an output redeemable with a single signature
    pub fn p2wpkh(pk: &key::PublicKey, network: Network) -> Address {
        let mut hash_engine = hash160::Hash::engine();
        pk.write_into(&mut hash_engine);

        Address {
            network: network,
            payload: Payload::WitnessProgram(
                // unwrap is safe as witness program is known to be correct as above
                WitnessProgram::new(
                    u5::try_from_u8(0).expect("0<32"),
                    hash160::Hash::from_engine(hash_engine)[..].to_vec(),
                    Address::bech_network(network),
                )
                .unwrap(),
            ),
        }
    }

    /// Create a pay to script address that embeds a witness pay to public key
    /// This is a segwit address type that looks familiar (as p2sh) to legacy clients
    pub fn p2shwpkh(pk: &key::PublicKey, network: Network) -> Address {
        let mut hash_engine = hash160::Hash::engine();
        pk.write_into(&mut hash_engine);

        let builder = script::Builder::new()
            .push_int(0)
            .push_slice(&hash160::Hash::from_engine(hash_engine)[..]);

        Address {
            network: network,
            payload: Payload::ScriptHash(hash160::Hash::hash(builder.into_script().as_bytes())),
        }
    }

    /// Create a witness pay to script hash address
    pub fn p2wsh(script: &script::Script, network: Network) -> Address {
        use bitcoin_hashes::sha256;
        use bitcoin_hashes::Hash;

        Address {
            network: network,
            payload: Payload::WitnessProgram(
                // unwrap is safe as witness program is known to be correct as above
                WitnessProgram::new(
                    u5::try_from_u8(0).expect("0<32"),
                    sha256::Hash::hash(&script[..])[..].to_vec(),
                    Address::bech_network(network),
                )
                .unwrap(),
            ),
        }
    }

    /// Create a pay to script address that embeds a witness pay to script hash address
    /// This is a segwit address type that looks familiar (as p2sh) to legacy clients
    pub fn p2shwsh(script: &script::Script, network: Network) -> Address {
        use bitcoin_hashes::hash160;
        use bitcoin_hashes::sha256;
        use bitcoin_hashes::Hash;

        let ws = script::Builder::new()
            .push_int(0)
            .push_slice(&sha256::Hash::hash(&script[..])[..])
            .into_script();

        Address {
            network: network,
            payload: Payload::ScriptHash(hash160::Hash::hash(&ws[..])),
        }
    }

    #[inline]
    /// convert Network to bech32 network (this should go away soon)
    fn bech_network(network: Network) -> bitcoin_bech32::constants::Network {
        match network {
            Network::Bitcoin => bitcoin_bech32::constants::Network::Bitcoin,
            Network::Testnet => bitcoin_bech32::constants::Network::Testnet,
            Network::Regtest => bitcoin_bech32::constants::Network::Regtest,

            // this should never actually happen, Liquid does not have bech32 addresses
            #[cfg(feature = "liquid")]
            Network::Liquid | Network::LiquidRegtest => bitcoin_bech32::constants::Network::Bitcoin,
        }
    }

    /// Generates a script pubkey spending to this address
    pub fn script_pubkey(&self) -> script::Script {
        match self.payload {
            Payload::PubkeyHash(ref hash) => script::Builder::new()
                .push_opcode(opcodes::all::OP_DUP)
                .push_opcode(opcodes::all::OP_HASH160)
                .push_slice(&hash[..])
                .push_opcode(opcodes::all::OP_EQUALVERIFY)
                .push_opcode(opcodes::all::OP_CHECKSIG),
            Payload::ScriptHash(ref hash) => script::Builder::new()
                .push_opcode(opcodes::all::OP_HASH160)
                .push_slice(&hash[..])
                .push_opcode(opcodes::all::OP_EQUAL),
            Payload::WitnessProgram(ref witprog) => script::Builder::new()
                .push_int(witprog.version().to_u8() as i64)
                .push_slice(witprog.program()),
        }
        .into_script()
    }
}

impl Display for Address {
    fn fmt(&self, fmt: &mut Formatter) -> fmt::Result {
        match self.payload {
            Payload::PubkeyHash(ref hash) => {
                let mut prefixed = [0; 21];
                prefixed[0] = match self.network {
                    Network::Bitcoin => 0,
                    Network::Testnet | Network::Regtest => 111,

                    #[cfg(feature = "liquid")]
                    Network::Liquid => 57,
                    #[cfg(feature = "liquid")]
                    Network::LiquidRegtest => 235,
                };
                prefixed[1..].copy_from_slice(&hash[..]);
                base58::check_encode_slice_to_fmt(fmt, &prefixed[..])
            }
            Payload::ScriptHash(ref hash) => {
                let mut prefixed = [0; 21];
                prefixed[0] = match self.network {
                    Network::Bitcoin => 5,
                    Network::Testnet | Network::Regtest => 196,

                    #[cfg(feature = "liquid")]
                    Network::Liquid => 39,
                    #[cfg(feature = "liquid")]
                    Network::LiquidRegtest => 75,
                };
                prefixed[1..].copy_from_slice(&hash[..]);
                base58::check_encode_slice_to_fmt(fmt, &prefixed[..])
            }
            Payload::WitnessProgram(ref witprog) => fmt.write_str(&witprog.to_address()),
        }
    }
}

impl FromStr for Address {
    type Err = encode::Error;

    fn from_str(s: &str) -> Result<Address, encode::Error> {
        // bech32 (note that upper or lowercase is allowed but NOT mixed case)
        if s.starts_with("bc1")
            || s.starts_with("BC1")
            || s.starts_with("tb1")
            || s.starts_with("TB1")
            || s.starts_with("bcrt1")
            || s.starts_with("BCRT1")
        {
            let witprog = WitnessProgram::from_address(s)?;
            let network = match witprog.network() {
                bitcoin_bech32::constants::Network::Bitcoin => Network::Bitcoin,
                bitcoin_bech32::constants::Network::Testnet => Network::Testnet,
                bitcoin_bech32::constants::Network::Regtest => Network::Regtest,
                _ => panic!("unknown network"),
            };
            if witprog.version().to_u8() != 0 {
                return Err(encode::Error::UnsupportedWitnessVersion(
                    witprog.version().to_u8(),
                ));
            }
            return Ok(Address {
                network: network,
                payload: Payload::WitnessProgram(witprog),
            });
        }

        if s.len() > 50 {
            return Err(encode::Error::Base58(base58::Error::InvalidLength(
                s.len() * 11 / 15,
            )));
        }

        // Base 58
        let data = base58::from_check(s)?;

        if data.len() != 21 {
            return Err(encode::Error::Base58(base58::Error::InvalidLength(
                data.len(),
            )));
        }

        let (network, payload) = match data[0] {
            0 => (
                Network::Bitcoin,
                Payload::PubkeyHash(hash160::Hash::from_slice(&data[1..]).unwrap()),
            ),
            5 => (
                Network::Bitcoin,
                Payload::ScriptHash(hash160::Hash::from_slice(&data[1..]).unwrap()),
            ),
            111 => (
                Network::Testnet,
                Payload::PubkeyHash(hash160::Hash::from_slice(&data[1..]).unwrap()),
            ),
            196 => (
                Network::Testnet,
                Payload::ScriptHash(hash160::Hash::from_slice(&data[1..]).unwrap()),
            ),
            #[cfg(feature = "liquid")]
            57 => (
                Network::Liquid,
                Payload::PubkeyHash(hash160::Hash::from_slice(&data[1..]).unwrap()),
            ),
            #[cfg(feature = "liquid")]
            39 => (
                Network::Liquid,
                Payload::ScriptHash(hash160::Hash::from_slice(&data[1..]).unwrap()),
            ),
            #[cfg(feature = "liquid")]
            235 => (
                Network::LiquidRegtest,
                Payload::PubkeyHash(hash160::Hash::from_slice(&data[1..]).unwrap()),
            ),
            #[cfg(feature = "liquid")]
            75 => (
                Network::LiquidRegtest,
                Payload::ScriptHash(hash160::Hash::from_slice(&data[1..]).unwrap()),
            ),
            x => {
                return Err(encode::Error::Base58(base58::Error::InvalidVersion(vec![
                    x,
                ])));
            }
        };

        Ok(Address {
            network: network,
            payload: payload,
        })
    }
}

impl ::std::fmt::Debug for Address {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        write!(f, "{}", self.to_string())
    }
}
