//! Bitcoin Knots BLAKE2b proof-of-work hardfork support.
//!
//! <https://github.com/bitcoinknots/bitcoin/pull/359> reshapes the block header:
//! the top bit of the version field, when set, switches the header from the
//! legacy 80-byte SHA256d-hashed form to a 164-byte form hashed with BLAKE2b.
//!
//! `bitcoin::block::Header` can only represent the legacy form, so this module
//! provides a drop-in replacement for it. Legacy headers keep round-tripping
//! and hashing exactly as before.
//!
//! The header type and its hash are adapted from Retropex's electrs fork
//! (<https://github.com/Retropex/electrs>, commit 4453cac6, MIT), which
//! mirrors `CBlockHeader::GetHash()` in Bitcoin Knots.

use bitcoin::block::Version;
use bitcoin::consensus::encode::{Decodable, Encodable, Error as EncodeError};
use bitcoin::hashes::{sha256, Hash, HashEngine};
#[cfg(test)]
use bitcoin::hex::DisplayHex;
use bitcoin::io::{Error as IoError, Read, Write};
use bitcoin::{BlockHash, CompactTarget, TxMerkleNode};

use blake2::digest::consts::U32;
use blake2::{Blake2b, Digest};

/// Set in the header's version field to indicate the extended (BLAKE2b) header.
const VERSION_HEADER_V2_FLAG: u32 = 0x8000_0000;

/// `flags` bit indicating that `time_offset` is subtracted from the on-wire time.
const FLAG_USE_TIME_OFFSET: u8 = 4;

/// Serialized size of a legacy header.
pub const HEADER_V1_SIZE: usize = 80;
/// Serialized size of an extended (BLAKE2b) header.
pub const HEADER_V2_SIZE: usize = 164;

/// A Bitcoin block header, in either the legacy or the extended (BLAKE2b) form.
///
/// The fields shared with the legacy header keep the names used by
/// `bitcoin::block::Header`, so that consumers that don't care about the
/// hardfork need no changes.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct BlockHeader {
    /// Block version, excluding the extended-header flag.
    pub version: Version,
    pub prev_blockhash: BlockHash,
    pub merkle_root: TxMerkleNode,
    /// Block time. Note that for extended headers this is not what appears on
    /// the wire; see [`BlockHeader::time_on_wire`].
    pub time: u32,
    pub bits: CompactTarget,
    pub nonce: u32,

    /// Whether this is an extended (BLAKE2b) header.
    pub header_v2: bool,
    /// Additional PoW/ASIC grinding nonces.
    pub nonce2: u32,
    pub nonce3: u32,
    /// Stratum v1 extranonce.
    pub extranonce: [u8; 16],
    pub time_offset: u32,
    /// Transaction count commitment (the fix for CVE-2017-12842).
    pub txcount: u16,
    pub flags: u8,
    pub xor_key_mask_clear_bits: u8,
    pub xor_key: [u8; 16],
    pub height: i32,
    /// Merge-mining hook.
    pub mm_rhs: [u8; 32],
}

impl BlockHeader {
    /// The version as it appears on the wire, including the extended-header flag.
    pub fn complete_version(&self) -> u32 {
        let version = self.version.to_consensus() as u32 & !VERSION_HEADER_V2_FLAG;
        if self.header_v2 {
            version | VERSION_HEADER_V2_FLAG
        } else {
            version
        }
    }

    /// The block time as it appears on the wire, which the extended header may
    /// carry offset by `time_offset`.
    pub fn time_on_wire(&self) -> u32 {
        if self.flags & FLAG_USE_TIME_OFFSET == 0 {
            self.time
        } else {
            self.time.wrapping_sub(self.time_offset)
        }
    }

    /// The serialized size of this header.
    pub fn size(&self) -> usize {
        if self.header_v2 {
            HEADER_V2_SIZE
        } else {
            HEADER_V1_SIZE
        }
    }

    pub fn block_hash(&self) -> BlockHash {
        if !self.header_v2 {
            // Historical algorithm and common case: SHA256d over the 80 bytes.
            let mut engine = BlockHash::engine();
            self.consensus_encode(&mut engine)
                .expect("engines don't error");
            return BlockHash::from_engine(engine);
        }
        self.blake2b_block_hash()
    }

    /// The BLAKE2b hash of an extended header, mirroring `CBlockHeader::GetHash()`.
    fn blake2b_block_hash(&self) -> BlockHash {
        // A pooling miner only learns `xor_key` once it finds a block, so the
        // header commits to the key's hash rather than to the key itself.
        let xor_key_hash = tagged_hash("Bitcoin block hash PoW XOR key", &[&self.xor_key]);

        let mut xor_key_mask = [0u8; 32];
        if self.xor_key != [0u8; 16] {
            xor_key_mask = tagged_hash("Bitcoin block hash PoW XOR mask", &[&self.xor_key]);
            // `clear_bits` is a u8, so this stays in bounds.
            let clear_bytes = usize::from(self.xor_key_mask_clear_bits / 8);
            xor_key_mask[..clear_bytes].fill(0);
            xor_key_mask[clear_bytes] &= 0xffu8 >> (self.xor_key_mask_clear_bits % 8);
        }

        let mut prevblock_ordered = self.prev_blockhash.to_byte_array();
        prevblock_ordered.reverse();
        let mut prevblock_hidden =
            tagged_hash("Bitcoin prevblock header, hashed", &[&prevblock_ordered]);

        // These fields are invisible to the mining machine, so the hasher cannot
        // brick itself at some future block version, time or difficulty.
        let h1 = tagged_hash(
            "Bitcoin block header 1",
            &[
                &self.complete_version().to_le_bytes(),
                &prevblock_ordered,
                &self.height.to_le_bytes(),
                self.merkle_root.as_byte_array(),
                &self.time_on_wire().to_le_bytes(),
                &[0], // reserved for extended 40-bit time
                &self.bits.to_consensus().to_le_bytes(),
                &u32::from(self.txcount).to_le_bytes(),
                &[self.flags, self.xor_key_mask_clear_bits],
                &xor_key_hash,
            ],
        );

        let h2 = tagged_hash("Merge-mining hook", &[&h1, &[0u8; 32], &self.mm_rhs]);

        // These fields get sent to mining machines over Stratum v1.
        let hash = blake2b_256(&[
            &[0u8; 4], // final 3 bytes are part of Sv1 "coinb1"
            &h2,
            &self.extranonce,
        ]);

        // Presumably the actual mining ASIC hardware sees these.
        let tail: [&[u8]; 5] = [
            &self.nonce.to_le_bytes(),
            &self.nonce2.to_le_bytes(),
            &self.time_offset.to_le_bytes(),
            &self.nonce3.to_le_bytes(),
            &hash,
        ];
        let hash = match self.flags & 3 {
            0 => {
                prevblock_hidden[..6].fill(0);
                blake2b_256(&[
                    &prevblock_hidden,
                    tail[0],
                    tail[1],
                    tail[2],
                    tail[3],
                    tail[4],
                ])
            }
            1 => blake2b_256(&[
                &self.nonce.to_le_bytes(),
                &self.nonce2.to_le_bytes(),
                &self.nonce3.to_le_bytes(),
                &self.time_offset.to_le_bytes(),
                &hash,
                &h2,
            ]),
            2 => blake2b_256(&[&[0u8; 48], &h2, tail[0], tail[1], tail[2], tail[3], tail[4]]),
            _ => blake2b_256(&[&[0u8; 80], &h2, tail[0], tail[1], tail[2], tail[3], tail[4]]),
        };

        let mut inner = [0u8; 32];
        for (i, byte) in inner.iter_mut().rev().enumerate() {
            *byte = hash[i] ^ xor_key_mask[i];
        }
        BlockHash::from_byte_array(inner)
    }
}

impl Encodable for BlockHeader {
    fn consensus_encode<W: Write + ?Sized>(&self, w: &mut W) -> Result<usize, IoError> {
        let mut len = 0;
        len += self.complete_version().consensus_encode(w)?;
        len += self.prev_blockhash.consensus_encode(w)?;
        len += self.merkle_root.consensus_encode(w)?;
        len += self.time_on_wire().consensus_encode(w)?;
        len += self.bits.consensus_encode(w)?;
        len += self.nonce.consensus_encode(w)?;
        if self.header_v2 {
            len += self.nonce2.consensus_encode(w)?;
            len += self.nonce3.consensus_encode(w)?;
            len += self.extranonce.consensus_encode(w)?;
            len += self.time_offset.consensus_encode(w)?;
            len += self.txcount.consensus_encode(w)?;
            len += self.flags.consensus_encode(w)?;
            len += self.xor_key_mask_clear_bits.consensus_encode(w)?;
            len += self.xor_key.consensus_encode(w)?;
            len += self.height.consensus_encode(w)?;
            len += self.mm_rhs.consensus_encode(w)?;
        }
        Ok(len)
    }
}

impl Decodable for BlockHeader {
    fn consensus_decode<R: Read + ?Sized>(r: &mut R) -> Result<Self, EncodeError> {
        let version = u32::consensus_decode(r)?;
        let mut header = BlockHeader {
            version: Version::from_consensus((version & !VERSION_HEADER_V2_FLAG) as i32),
            prev_blockhash: BlockHash::consensus_decode(r)?,
            merkle_root: TxMerkleNode::consensus_decode(r)?,
            time: u32::consensus_decode(r)?, // on-wire time, adjusted below
            bits: CompactTarget::consensus_decode(r)?,
            nonce: u32::consensus_decode(r)?,

            header_v2: version & VERSION_HEADER_V2_FLAG != 0,
            nonce2: 0,
            nonce3: 0,
            extranonce: [0u8; 16],
            time_offset: 0,
            txcount: 0,
            flags: 0,
            xor_key_mask_clear_bits: 0,
            xor_key: [0u8; 16],
            height: 0,
            mm_rhs: [0u8; 32],
        };
        if header.header_v2 {
            header.nonce2 = u32::consensus_decode(r)?;
            header.nonce3 = u32::consensus_decode(r)?;
            header.extranonce = <[u8; 16]>::consensus_decode(r)?;
            header.time_offset = u32::consensus_decode(r)?;
            header.txcount = u16::consensus_decode(r)?;
            header.flags = u8::consensus_decode(r)?;
            header.xor_key_mask_clear_bits = u8::consensus_decode(r)?;
            header.xor_key = <[u8; 16]>::consensus_decode(r)?;
            header.height = i32::consensus_decode(r)?;
            header.mm_rhs = <[u8; 32]>::consensus_decode(r)?;
        }
        if header.flags & FLAG_USE_TIME_OFFSET != 0 {
            header.time = header.time.wrapping_add(header.time_offset);
        }
        Ok(header)
    }
}

impl From<bitcoin::block::Header> for BlockHeader {
    fn from(h: bitcoin::block::Header) -> Self {
        BlockHeader {
            version: h.version,
            prev_blockhash: h.prev_blockhash,
            merkle_root: h.merkle_root,
            time: h.time,
            bits: h.bits,
            nonce: h.nonce,
            header_v2: false,
            nonce2: 0,
            nonce3: 0,
            extranonce: [0u8; 16],
            time_offset: 0,
            txcount: 0,
            flags: 0,
            xor_key_mask_clear_bits: 0,
            xor_key: [0u8; 16],
            height: 0,
            mm_rhs: [0u8; 32],
        }
    }
}

/// Walk a serialized block through a `bitcoin_slices` visitor.
///
/// `bsl::Block::visit` assumes an 80-byte header, so it cannot parse a block
/// with an extended header. This reads the header in either form, then the
/// transaction count, then each transaction through the visitor, with the
/// same `visit_block_begin` call and the same `Err(VisitBreak)` on an early
/// stop that `bsl::Block::visit` gives. `visit_block_header` is not called,
/// since an extended header has no `bsl` representation; nothing in electrs
/// implements it.
pub fn visit_block<V: bitcoin_slices::Visitor>(
    block: &[u8],
    visitor: &mut V,
) -> Result<BlockHeader, bitcoin_slices::Error> {
    use bitcoin::consensus::encode::VarInt;
    use bitcoin_slices::{bsl, Error, Visit};

    let header = BlockHeader::consensus_decode(&mut &block[..]).map_err(|_| Error::MoreBytesNeeded)?;
    let mut cursor = &block[header.size()..];
    let txcount = VarInt::consensus_decode(&mut cursor)
        .map_err(|_| Error::MoreBytesNeeded)?
        .0 as usize;
    visitor.visit_block_begin(txcount);
    for _ in 0..txcount {
        let parsed = bsl::Transaction::visit(cursor, visitor)?;
        cursor = parsed.remaining();
    }
    Ok(header)
}

/// BIP340-style tagged SHA256, as Bitcoin Core's `TaggedHash`.
fn tagged_hash(tag: &str, parts: &[&[u8]]) -> [u8; 32] {
    let tag_hash = sha256::Hash::hash(tag.as_bytes());
    let mut engine = sha256::Hash::engine();
    engine.input(tag_hash.as_byte_array());
    engine.input(tag_hash.as_byte_array());
    for part in parts {
        engine.input(part);
    }
    sha256::Hash::from_engine(engine).to_byte_array()
}

fn blake2b_256(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Blake2b::<U32>::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// (serialized header, block hash) vectors from Knots'
/// `src/test/data/block_header_v2.json`, covering all four ASIC profiles.

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::consensus::{deserialize, serialize};
    use bitcoin::hex::FromHex;

    #[test]
    fn blake2b_256_test_vectors() {
        // RFC 7693 style checks for the 32-byte output
        assert_eq!(
            blake2b_256(&[b""]).to_lower_hex_string(),
            "0e5751c026e543b2e8ab2eb06099daa1d1e5df47778f7787faab45cdf12fe3a8"
        );
        assert_eq!(
            blake2b_256(&[b"abc"]).to_lower_hex_string(),
            "bddd813c634239723171ef3fee98579b94964e3bb1cb3e427262c8c068d52319"
        );
    }

    #[test]
    fn legacy_header_unchanged() {
        // regtest genesis header: 80 bytes, SHA256d hash, no v2 flag
        let genesis = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest);
        let ours: BlockHeader = genesis.header.into();
        assert!(!ours.header_v2);
        assert_eq!(ours.size(), HEADER_V1_SIZE);
        assert_eq!(serialize(&ours), serialize(&genesis.header));
        assert_eq!(ours.block_hash(), genesis.header.block_hash());
        let back: BlockHeader = deserialize(&serialize(&genesis.header)).unwrap();
        assert_eq!(back, ours);
    }

    /// Bitcoin Knots' own vectors (src/test/data/block_header_v2.json at
    /// v29.4.1.knots20260508rc4): the serialized header and the resulting
    /// block hash in digest order.
    #[test]
    fn knots_header_v2_vectors() {
        let json: serde_json::Value = serde_json::from_str(include_str!("knots_vectors.json")).unwrap();
        let headers = json["headers"].as_array().unwrap();
        assert!(headers.len() >= 5);
        for v in headers {
            let name = v["name"].as_str().unwrap();
            let bytes = Vec::<u8>::from_hex(v["serialized"].as_str().unwrap()).unwrap();
            assert_eq!(bytes.len(), HEADER_V2_SIZE, "{}", name);
            let header: BlockHeader = deserialize(&bytes).unwrap_or_else(|e| panic!("{}: {}", name, e));
            assert!(header.header_v2, "{}", name);
            assert_eq!(serialize(&header), bytes, "{}: round trip", name);
            assert_eq!(header.block_hash().to_string(), v["block_hash"].as_str().unwrap(), "{}: block hash", name);
        }
    }

    /// Random headers hashed by an independent Python implementation
    /// (hdrv2.py). Set KNOTS_RANDOM_VECTORS to the JSON file to run it.
    #[test]
    fn random_vectors_differential() {
        let path = match std::env::var("KNOTS_RANDOM_VECTORS") {
            Ok(p) => p,
            Err(_) => return,
        };
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let vectors = json.as_array().unwrap();
        assert!(vectors.len() >= 100);
        for (i, v) in vectors.iter().enumerate() {
            let bytes = Vec::<u8>::from_hex(v["serialized"].as_str().unwrap()).unwrap();
            let header: BlockHeader = deserialize(&bytes).unwrap_or_else(|e| panic!("vector {}: {}", i, e));
            assert_eq!(serialize(&header), bytes, "vector {}: round trip", i);
            assert_eq!(header.block_hash().to_string(), v["block_hash"].as_str().unwrap(), "vector {}", i);
        }
    }
}
