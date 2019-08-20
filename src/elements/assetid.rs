// Copeid from https://github.com/ElementsProject/rust-elements/pull/19 pending merge

use bitcoin::hashes::{hex, sha256, sha256d, Hash, HashEngine};
use elements::encode::Encodable;
use elements::OutPoint;

/// The zero hash.
const ZERO32: [u8; 32] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
/// The one hash.
const ONE32: [u8; 32] = [
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
/// The two hash.
const TWO32: [u8; 32] = [
    2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// An issued asset ID.
#[derive(Copy, Clone, PartialEq, Eq, Default, PartialOrd, Ord, Hash)]
pub struct AssetId(sha256::Midstate);

impl AssetId {
    /// Create an [AssetId] from its inner type.
    pub fn from_inner(midstate: sha256::Midstate) -> AssetId {
        AssetId(midstate)
    }

    /// Convert the [AssetId] into its inner type.
    pub fn into_inner(self) -> sha256::Midstate {
        self.0
    }

    /// Generate the asset entropy from the issuance prevout and the contract hash.
    pub fn generate_asset_entropy(
        prevout: OutPoint,
        contract_hash: sha256::Hash,
    ) -> sha256::Midstate {
        // E : entropy
        // I : prevout
        // C : contract
        // E = H( H(I) || H(C) )
        fast_merkle_root(&[
            outpoint_hash(&prevout).into_inner(),
            contract_hash.into_inner(),
        ])
    }

    /// Calculate the asset ID from the asset entropy.
    pub fn from_entropy(entropy: sha256::Midstate) -> AssetId {
        // H_a : asset tag
        // E   : entropy
        // H_a = H( E || 0 )
        AssetId(fast_merkle_root(&[entropy.into_inner(), ZERO32]))
    }

    /// Calculate the reissuance token asset ID from the asset entropy.
    pub fn reissuance_token_from_entropy(entropy: sha256::Midstate, confidential: bool) -> AssetId {
        // H_a : asset reissuance tag
        // E   : entropy
        // if not fConfidential:
        //     H_a = H( E || 1 )
        // else
        //     H_a = H( E || 2 )
        let second = match confidential {
            false => ONE32,
            true => TWO32,
        };
        AssetId(fast_merkle_root(&[entropy.into_inner(), second]))
    }
}

fn outpoint_hash(out: &OutPoint) -> sha256d::Hash {
    let mut enc = sha256d::Hash::engine();
    out.consensus_encode(&mut enc).unwrap();
    sha256d::Hash::from_engine(enc)
}

impl hex::FromHex for AssetId {
    fn from_byte_iter<I>(iter: I) -> Result<Self, hex::Error>
    where
        I: Iterator<Item = Result<u8, hex::Error>> + ExactSizeIterator + DoubleEndedIterator,
    {
        sha256::Midstate::from_byte_iter(iter).map(AssetId)
    }
}

impl ::std::fmt::Display for AssetId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        ::std::fmt::Display::fmt(&self.0, f)
    }
}

impl ::std::fmt::Debug for AssetId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        ::std::fmt::Display::fmt(&self, f)
    }
}

impl ::std::fmt::LowerHex for AssetId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        ::std::fmt::LowerHex::fmt(&self.0, f)
    }
}

#[cfg(feature = "serde")]
impl ::serde::Serialize for AssetId {
    fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use bitcoin::hashes::hex::ToHex;
        if s.is_human_readable() {
            s.serialize_str(&self.to_hex())
        } else {
            s.serialize_bytes(&self.0[..])
        }
    }
}

#[cfg(feature = "serde")]
impl<'de> ::serde::Deserialize<'de> for AssetId {
    fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<AssetId, D::Error> {
        use bitcoin::hashes::hex::FromHex;
        use bitcoin::hashes::sha256;

        if d.is_human_readable() {
            struct HexVisitor;

            impl<'de> ::serde::de::Visitor<'de> for HexVisitor {
                type Value = AssetId;

                fn expecting(&self, formatter: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
                    formatter.write_str("an ASCII hex string")
                }

                fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
                where
                    E: ::serde::de::Error,
                {
                    if let Ok(hex) = ::std::str::from_utf8(v) {
                        AssetId::from_hex(hex).map_err(E::custom)
                    } else {
                        return Err(E::invalid_value(::serde::de::Unexpected::Bytes(v), &self));
                    }
                }

                fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
                where
                    E: ::serde::de::Error,
                {
                    AssetId::from_hex(v).map_err(E::custom)
                }
            }

            d.deserialize_str(HexVisitor)
        } else {
            struct BytesVisitor;

            impl<'de> ::serde::de::Visitor<'de> for BytesVisitor {
                type Value = AssetId;

                fn expecting(&self, formatter: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
                    formatter.write_str("a bytestring")
                }

                fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
                where
                    E: ::serde::de::Error,
                {
                    if v.len() != 32 {
                        Err(E::invalid_length(v.len(), &stringify!($len)))
                    } else {
                        let mut ret = [0; 32];
                        ret.copy_from_slice(v);
                        Ok(AssetId(sha256::Midstate::from_inner(ret)))
                    }
                }
            }

            d.deserialize_bytes(BytesVisitor)
        }
    }
}

/// Calculate a single sha256 midstate hash of the given left and right leaves.
#[inline]
fn sha256midstate(left: &[u8], right: &[u8]) -> sha256::Midstate {
    let mut engine = sha256::Hash::engine();
    engine.input(left);
    engine.input(right);
    engine.midstate()
}

/// Compute the Merkle root of the give hashes using mid-state only.
/// The inputs must be byte slices of length 32.
/// Note that the merkle root calculated with this method is not the same as the
/// one computed by a normal SHA256(d) merkle root.
pub fn fast_merkle_root(leaves: &[[u8; 32]]) -> sha256::Midstate {
    let mut result_hash = Default::default();
    // Implementation based on ComputeFastMerkleRoot method in Elements Core.
    if leaves.is_empty() {
        return result_hash;
    }

    // inner is an array of eagerly computed subtree hashes, indexed by tree
    // level (0 being the leaves).
    // For example, when count is 25 (11001 in binary), inner[4] is the hash of
    // the first 16 leaves, inner[3] of the next 8 leaves, and inner[0] equal to
    // the last leaf. The other inner entries are undefined.
    //
    // First process all leaves into 'inner' values.
    let mut inner: [sha256::Midstate; 32] = Default::default();
    let mut count: u32 = 0;
    while (count as usize) < leaves.len() {
        let mut temp_hash = sha256::Midstate::from_inner(leaves[count as usize]);
        count += 1;
        // For each of the lower bits in count that are 0, do 1 step. Each
        // corresponds to an inner value that existed before processing the
        // current leaf, and each needs a hash to combine it.
        let mut level = 0;
        while count & (1u32 << level) == 0 {
            temp_hash = sha256midstate(&inner[level][..], &temp_hash[..]);
            level += 1;
        }
        // Store the resulting hash at inner position level.
        inner[level] = temp_hash;
    }

    // Do a final 'sweep' over the rightmost branch of the tree to process
    // odd levels, and reduce everything to a single top value.
    // Level is the level (counted from the bottom) up to which we've sweeped.
    //
    // As long as bit number level in count is zero, skip it. It means there
    // is nothing left at this level.
    let mut level = 0;
    while count & (1u32 << level) == 0 {
        level += 1;
    }
    result_hash = inner[level];

    while count != (1u32 << level) {
        // If we reach this point, hash is an inner value that is not the top.
        // We combine it with itself (Bitcoin's special rule for odd levels in
        // the tree) to produce a higher level one.

        // Increment count to the value it would have if two entries at this
        // level had existed and propagate the result upwards accordingly.
        count += 1 << level;
        level += 1;
        while count & (1u32 << level) == 0 {
            result_hash = sha256midstate(&inner[level][..], &result_hash[..]);
            level += 1;
        }
    }
    // Return result.
    result_hash
}
