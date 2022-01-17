use anyhow::Result;

use std::convert::TryFrom;

use bitcoin::{
    consensus::encode::{deserialize, serialize, Decodable, Encodable},
    hashes::{borrow_slice_impl, hash_newtype, hex_fmt_impl, index_impl, serde_impl, sha256, Hash},
    BlockHash, BlockHeader, OutPoint, Script, Txid,
};

use crate::db;

macro_rules! impl_consensus_encoding {
    ($thing:ident, $($field:ident),+) => (
        impl Encodable for $thing {
            #[inline]
            fn consensus_encode<S: ::std::io::Write>(
                &self,
                mut s: S,
            ) -> Result<usize, std::io::Error> {
                let mut len = 0;
                $(len += self.$field.consensus_encode(&mut s)?;)+
                Ok(len)
            }
        }

        impl Decodable for $thing {
            #[inline]
            fn consensus_decode<D: ::std::io::Read>(
                mut d: D,
            ) -> Result<$thing, bitcoin::consensus::encode::Error> {
                Ok($thing {
                    $($field: Decodable::consensus_decode(&mut d)?),+
                })
            }
        }
    );
}

const HASH_PREFIX_LEN: usize = 8;

type HashPrefix = [u8; HASH_PREFIX_LEN];

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Copy, Clone, PartialOrd, Ord)]
pub(crate) struct FilePosition {
    #[serde(rename = "file")]
    pub file_id: u16, // blk*.dat file index (<3k as of 2022/01)
    #[serde(rename = "data")]
    pub offset: u32, // offset within a single blk*.dat file (~128MB as 2022/01)
}

impl FilePosition {
    pub fn with_offset(&self, offset: u32) -> Self {
        Self {
            file_id: self.file_id,
            offset,
        }
    }
}

impl_consensus_encoding!(FilePosition, file_id, offset);

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct HashPrefixRow {
    prefix: [u8; HASH_PREFIX_LEN],
    pos: FilePosition, // transaction confirmed height
}

impl HashPrefixRow {
    pub(crate) fn to_db_row(&self) -> db::Row {
        serialize(self).into_boxed_slice()
    }

    pub(crate) fn from_db_row(row: &[u8]) -> Self {
        deserialize(row).expect("bad HashPrefixRow")
    }

    pub fn pos(&self) -> FilePosition {
        self.pos
    }
}

impl_consensus_encoding!(HashPrefixRow, prefix, pos);

hash_newtype!(
    ScriptHash,
    sha256::Hash,
    32,
    doc = "https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-basics.html#script-hashes",
    true
);

impl ScriptHash {
    pub fn new(script: &Script) -> Self {
        ScriptHash::hash(&script[..])
    }

    fn prefix(&self) -> HashPrefix {
        let mut prefix = HashPrefix::default();
        prefix.copy_from_slice(&self.0[..HASH_PREFIX_LEN]);
        prefix
    }
}

pub(crate) struct ScriptHashRow;

impl ScriptHashRow {
    pub(crate) fn scan_prefix(scripthash: ScriptHash) -> Box<[u8]> {
        scripthash.0[..HASH_PREFIX_LEN].to_vec().into_boxed_slice()
    }

    pub(crate) fn row(scripthash: ScriptHash, pos: FilePosition) -> HashPrefixRow {
        HashPrefixRow {
            prefix: scripthash.prefix(),
            pos,
        }
    }
}

// ***************************************************************************

hash_newtype!(
    StatusHash,
    sha256::Hash,
    32,
    doc = "https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-basics.html#status",
    false
);

// ***************************************************************************

fn spending_prefix(prev: OutPoint) -> HashPrefix {
    let txid_prefix = <[u8; HASH_PREFIX_LEN]>::try_from(&prev.txid[..HASH_PREFIX_LEN]).unwrap();
    let value = u64::from_be_bytes(txid_prefix);
    let value = value.wrapping_add(prev.vout.into());
    value.to_be_bytes()
}

pub(crate) struct SpendingPrefixRow;

impl SpendingPrefixRow {
    pub(crate) fn scan_prefix(outpoint: OutPoint) -> Box<[u8]> {
        Box::new(spending_prefix(outpoint))
    }

    pub(crate) fn row(outpoint: OutPoint, pos: FilePosition) -> HashPrefixRow {
        HashPrefixRow {
            prefix: spending_prefix(outpoint),
            pos,
        }
    }
}

// ***************************************************************************

fn txid_prefix(txid: &Txid) -> HashPrefix {
    let mut prefix = [0u8; HASH_PREFIX_LEN];
    prefix.copy_from_slice(&txid[..HASH_PREFIX_LEN]);
    prefix
}

pub(crate) struct TxidRow;

impl TxidRow {
    pub(crate) fn scan_prefix(txid: Txid) -> Box<[u8]> {
        Box::new(txid_prefix(&txid))
    }

    pub(crate) fn row(txid: Txid, pos: FilePosition) -> HashPrefixRow {
        HashPrefixRow {
            prefix: txid_prefix(&txid),
            pos,
        }
    }
}

// ***************************************************************************

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct HeaderRow {
    pub(crate) header: BlockHeader,
    pub(crate) hash: BlockHash,
    pub(crate) pos: FilePosition,
    pub(crate) size: u32, // block size in bytes
}

impl_consensus_encoding!(HeaderRow, header, hash, pos, size);

impl HeaderRow {
    pub(crate) fn new(header: BlockHeader, pos: FilePosition, size: u32) -> Self {
        let hash = header.block_hash();
        Self {
            header,
            hash,
            pos,
            size,
        }
    }

    pub(crate) fn to_db_row(&self) -> db::Row {
        serialize(self).into_boxed_slice()
    }

    pub(crate) fn from_db_row(row: &[u8]) -> Self {
        deserialize(row).expect("bad HeaderRow")
    }
}

#[cfg(test)]
mod tests {
    use super::{spending_prefix, FilePosition, HashPrefixRow, ScriptHash, ScriptHashRow, TxidRow};
    use bitcoin::{hashes::hex::ToHex, Address, OutPoint, Txid};
    use serde_json::{from_str, json};

    use std::str::FromStr;

    #[test]
    fn test_scripthash_serde() {
        let hex = "\"4b3d912c1523ece4615e91bf0d27381ca72169dbf6b1c2ffcc9f92381d4984a3\"";
        let scripthash: ScriptHash = from_str(&hex).unwrap();
        assert_eq!(format!("\"{}\"", scripthash), hex);
        assert_eq!(json!(scripthash).to_string(), hex);
    }

    #[test]
    fn test_scripthash_row() {
        let hex = "\"4b3d912c1523ece4615e91bf0d27381ca72169dbf6b1c2ffcc9f92381d4984a3\"";
        let scripthash: ScriptHash = from_str(&hex).unwrap();
        let row1 = ScriptHashRow::row(
            scripthash,
            FilePosition {
                file_id: 0xabcd,
                offset: 0x12345678,
            },
        );
        let db_row = row1.to_db_row();
        assert_eq!(db_row[..].to_hex(), "a384491d38929fcccdab78563412");
        let row2 = HashPrefixRow::from_db_row(&db_row);
        assert_eq!(row1, row2);
    }

    #[test]
    fn test_txid_row() {
        let hex = "\"4b3d912c1523ece4615e91bf0d27381ca72169dbf6b1c2ffcc9f92381d4984a3\"";
        let txid: Txid = from_str(&hex).unwrap();
        let row1 = TxidRow::row(
            txid,
            FilePosition {
                file_id: 0xabcd,
                offset: 0x12345678,
            },
        );
        let db_row = row1.to_db_row();
        assert_eq!(db_row[..].to_hex(), "a384491d38929fcccdab78563412");
        let row2 = HashPrefixRow::from_db_row(&db_row);
        assert_eq!(row1, row2);
    }

    #[test]
    fn test_scripthash() {
        let addr = Address::from_str("1KVNjD3AAnQ3gTMqoTKcWFeqSFujq9gTBT").unwrap();
        let scripthash = ScriptHash::new(&addr.script_pubkey());
        assert_eq!(
            scripthash.to_hex(),
            "00dfb264221d07712a144bda338e89237d1abd2db4086057573895ea2659766a"
        );
    }

    #[test]
    fn test_spending_prefix() {
        let hex = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let txid = Txid::from_str(hex).unwrap();

        assert_eq!(
            spending_prefix(OutPoint { txid, vout: 0 }),
            [31, 30, 29, 28, 27, 26, 25, 24]
        );
        assert_eq!(
            spending_prefix(OutPoint { txid, vout: 10 }),
            [31, 30, 29, 28, 27, 26, 25, 34]
        );
        assert_eq!(
            spending_prefix(OutPoint { txid, vout: 255 }),
            [31, 30, 29, 28, 27, 26, 26, 23]
        );
        assert_eq!(
            spending_prefix(OutPoint { txid, vout: 256 }),
            [31, 30, 29, 28, 27, 26, 26, 24]
        );
    }
}
