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

// **
// Copied from rust-bitcoin, with modifications to support Liquid-encoded addresses.
// **

//! Addresses
//!
//! Support for ordinary base58 Bitcoin addresses and private keys
//!

use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use bitcoin_bech32::{self, u5, WitnessProgram};
use secp256k1::key::PublicKey;

use bitcoin::blockdata::opcodes;
use bitcoin::blockdata::script;
use bitcoin::consensus::encode;
use bitcoin::util::base58;
use bitcoin::util::hash::Hash160;

use crate::chain::Network;

/// The method used to produce an address
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Payload {
    /// pay-to-pubkey
    Pubkey(PublicKey),
    /// pay-to-pkhash address
    PubkeyHash(Hash160),
    /// P2SH address
    ScriptHash(Hash160),
    /// Segwit address
    WitnessProgram(WitnessProgram),
}

// Originally was: #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[derive(Clone, PartialEq, Hash)]
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
    pub fn p2pkh(pk: &PublicKey, network: Network) -> Address {
        Address {
            network: network,
            payload: Payload::PubkeyHash(Hash160::from_data(&pk.serialize()[..])),
        }
    }

    /// Creates a pay to uncompressed public key hash address from a public key
    /// This address type is discouraged as it uses more space but otherwise equivalent to p2pkh
    /// therefore only adds ambiguity
    #[inline]
    pub fn p2upkh(pk: &PublicKey, network: Network) -> Address {
        Address {
            network: network,
            payload: Payload::PubkeyHash(Hash160::from_data(&pk.serialize_uncompressed()[..])),
        }
    }

    /// Creates a pay to public key address from a public key
    /// This address type was used in the early history of Bitcoin.
    /// Satoshi's coins are still on addresses of this type.
    #[inline]
    pub fn p2pk(pk: &PublicKey, network: Network) -> Address {
        Address {
            network: network,
            payload: Payload::Pubkey(*pk),
        }
    }

    /// Creates a pay to script hash P2SH address from a script
    /// This address type was introduced with BIP16 and is the popular ty implement multi-sig these days.
    #[inline]
    pub fn p2sh(script: &script::Script, network: Network) -> Address {
        Address {
            network: network,
            payload: Payload::ScriptHash(Hash160::from_data(&script[..])),
        }
    }

    /// Create a witness pay to public key address from a public key
    /// This is the native segwit address type for an output redemable with a single signature
    pub fn p2wpkh(pk: &PublicKey, network: Network) -> Address {
        Address {
            network: network,
            payload: Payload::WitnessProgram(
                // unwrap is safe as witness program is known to be correct as above
                WitnessProgram::new(
                    u5::try_from_u8(0).expect("0<32"),
                    Hash160::from_data(&pk.serialize()[..])[..].to_vec(),
                    Address::bech_network(network),
                )
                .unwrap(),
            ),
        }
    }

    /// Create a pay to script address that embeds a witness pay to public key
    /// This is a segwit address type that looks familiar (as p2sh) to legacy clients
    pub fn p2shwpkh(pk: &PublicKey, network: Network) -> Address {
        let builder = script::Builder::new()
            .push_int(0)
            .push_slice(&Hash160::from_data(&pk.serialize()[..])[..]);
        Address {
            network: network,
            payload: Payload::ScriptHash(Hash160::from_data(builder.into_script().as_bytes())),
        }
    }

    /// Create a witness pay to script hash address
    pub fn p2wsh(script: &script::Script, network: Network) -> Address {
        use crypto::digest::Digest;
        use crypto::sha2::Sha256;

        let mut digest = Sha256::new();
        digest.input(script.as_bytes());
        let mut d = [0u8; 32];
        digest.result(&mut d);

        Address {
            network: network,
            payload: Payload::WitnessProgram(
                // unwrap is safe as witness program is known to be correct as above
                WitnessProgram::new(
                    u5::try_from_u8(0).expect("0<32"),
                    d.to_vec(),
                    Address::bech_network(network),
                )
                .unwrap(),
            ),
        }
    }

    /// Create a pay to script address that embeds a witness pay to script hash address
    /// This is a segwit address type that looks familiar (as p2sh) to legacy clients
    pub fn p2shwsh(script: &script::Script, network: Network) -> Address {
        use crypto::digest::Digest;
        use crypto::sha2::Sha256;

        let mut digest = Sha256::new();
        digest.input(script.as_bytes());
        let mut d = [0u8; 32];
        digest.result(&mut d);
        let ws = script::Builder::new()
            .push_int(0)
            .push_slice(&d)
            .into_script();

        Address {
            network: network,
            payload: Payload::ScriptHash(Hash160::from_data(ws.as_bytes())),
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
            #[cfg(feature="liquid")]
            Network::Liquid | Network::LiquidRegtest => {
                bitcoin_bech32::constants::Network::Bitcoin
            }
        }
    }

    /// Generates a script pubkey spending to this address
    pub fn script_pubkey(&self) -> script::Script {
        match self.payload {
            Payload::Pubkey(ref pk) => script::Builder::new()
                .push_slice(&pk.serialize_uncompressed()[..])
                .push_opcode(opcodes::All::OP_CHECKSIG),
            Payload::PubkeyHash(ref hash) => script::Builder::new()
                .push_opcode(opcodes::All::OP_DUP)
                .push_opcode(opcodes::All::OP_HASH160)
                .push_slice(&hash[..])
                .push_opcode(opcodes::All::OP_EQUALVERIFY)
                .push_opcode(opcodes::All::OP_CHECKSIG),
            Payload::ScriptHash(ref hash) => script::Builder::new()
                .push_opcode(opcodes::All::OP_HASH160)
                .push_slice(&hash[..])
                .push_opcode(opcodes::All::OP_EQUAL),
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
            // note: serialization for pay-to-pk is defined, but is irreversible
            Payload::Pubkey(ref pk) => {
                let hash = &Hash160::from_data(&pk.serialize_uncompressed()[..]);
                let mut prefixed = [0; 21];
                prefixed[0] = match self.network {
                    Network::Bitcoin => 0,
                    Network::Testnet | Network::Regtest => 111,

                    #[cfg(feature="liquid")]
                    Network::Liquid => 57,
                    #[cfg(feature="liquid")]
                    Network::LiquidRegtest => 196,
                };
                prefixed[1..].copy_from_slice(&hash[..]);
                base58::check_encode_slice_to_fmt(fmt, &prefixed[..])
            }
            Payload::PubkeyHash(ref hash) => {
                let mut prefixed = [0; 21];
                prefixed[0] = match self.network {
                    Network::Bitcoin => 0,
                    Network::Testnet | Network::Regtest => 111,

                    #[cfg(feature="liquid")]
                    Network::Liquid => 57,
                    #[cfg(feature="liquid")]
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

                    #[cfg(feature="liquid")]
                    Network::Liquid => 39,
                    #[cfg(feature="liquid")]
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
                Payload::PubkeyHash(Hash160::from(&data[1..])),
            ),
            5 => (
                Network::Bitcoin,
                Payload::ScriptHash(Hash160::from(&data[1..])),
            ),
            111 => (
                Network::Testnet,
                Payload::PubkeyHash(Hash160::from(&data[1..])),
            ),
            196 => (
                Network::Testnet,
                Payload::ScriptHash(Hash160::from(&data[1..])),
            ),
            #[cfg(feature="liquid")]
            57 => (
                Network::Liquid,
                Payload::PubkeyHash(Hash160::from(&data[1..])),
            ),
            #[cfg(feature="liquid")]
            39 => (
                Network::Liquid,
                Payload::ScriptHash(Hash160::from(&data[1..])),
            ),
            #[cfg(feature="liquid")]
            235 => (
                Network::LiquidRegtest,
                Payload::PubkeyHash(Hash160::from(&data[1..])),
            ),
            #[cfg(feature="liquid")]
            75 => (
                Network::LiquidRegtest,
                Payload::ScriptHash(Hash160::from(&data[1..])),
            ),
            x => {
                return Err(encode::Error::Base58(base58::Error::InvalidVersion(vec![
                    x,
                ])))
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
