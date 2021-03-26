use anyhow::Result;

use std::convert::TryInto;

use bitcoin::{
    consensus::encode::{deserialize, serialize, Decodable, Encodable},
    hashes::{borrow_slice_impl, hash_newtype, hex_fmt_impl, index_impl, serde_impl, sha256, Hash},
    BlockHeader, OutPoint, Script, Txid,
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

    fn prefix(&self) -> ScriptHashPrefix {
        let mut prefix = [0u8; PREFIX_LEN];
        prefix.copy_from_slice(&self.0[..PREFIX_LEN]);
        ScriptHashPrefix { prefix }
    }
}

const PREFIX_LEN: usize = 8;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct ScriptHashPrefix {
    prefix: [u8; PREFIX_LEN],
}

impl_consensus_encoding!(ScriptHashPrefix, prefix);

type Height = u32;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct ScriptHashRow {
    prefix: ScriptHashPrefix,
    height: Height, // transaction confirmed height
}

impl_consensus_encoding!(ScriptHashRow, prefix, height);

impl ScriptHashRow {
    pub(crate) fn scan_prefix(scripthash: ScriptHash) -> Box<[u8]> {
        scripthash.0[..PREFIX_LEN].to_vec().into_boxed_slice()
    }

    pub(crate) fn new(scripthash: ScriptHash, height: usize) -> Self {
        Self {
            prefix: scripthash.prefix(),
            height: height.try_into().expect("invalid height"),
        }
    }

    pub(crate) fn to_db_row(&self) -> db::Row {
        serialize(self).into_boxed_slice()
    }

    pub(crate) fn from_db_row(row: &[u8]) -> Self {
        deserialize(&row).expect("bad ScriptHashRow")
    }

    pub(crate) fn height(&self) -> usize {
        self.height.try_into().expect("invalid height")
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

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct SpendingPrefix {
    prefix: [u8; PREFIX_LEN],
}

impl_consensus_encoding!(SpendingPrefix, prefix);

fn spending_prefix(prev: OutPoint) -> SpendingPrefix {
    let txid_prefix = &prev.txid[..PREFIX_LEN];
    let value = u64::from_be_bytes(txid_prefix.try_into().unwrap());
    let value = value.wrapping_add(prev.vout.into());
    SpendingPrefix {
        prefix: value.to_be_bytes(),
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct SpendingPrefixRow {
    prefix: SpendingPrefix,
    height: Height, // transaction confirmed height
}

impl_consensus_encoding!(SpendingPrefixRow, prefix, height);

impl SpendingPrefixRow {
    pub(crate) fn scan_prefix(outpoint: OutPoint) -> Box<[u8]> {
        Box::new(spending_prefix(outpoint).prefix)
    }

    pub(crate) fn new(outpoint: OutPoint, height: usize) -> Self {
        Self {
            prefix: spending_prefix(outpoint),
            height: height.try_into().expect("invalid height"),
        }
    }

    pub(crate) fn to_db_row(&self) -> db::Row {
        serialize(self).into_boxed_slice()
    }

    pub(crate) fn from_db_row(row: &[u8]) -> Self {
        deserialize(&row).expect("bad SpendingPrefixRow")
    }

    pub(crate) fn height(&self) -> usize {
        self.height.try_into().expect("invalid height")
    }
}

// ***************************************************************************

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct TxidPrefix {
    prefix: [u8; PREFIX_LEN],
}

impl_consensus_encoding!(TxidPrefix, prefix);

fn txid_prefix(txid: &Txid) -> TxidPrefix {
    let mut prefix = [0u8; PREFIX_LEN];
    prefix.copy_from_slice(&txid[..PREFIX_LEN]);
    TxidPrefix { prefix }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct TxidRow {
    prefix: TxidPrefix,
    height: Height, // transaction confirmed height
}

impl_consensus_encoding!(TxidRow, prefix, height);

impl TxidRow {
    pub(crate) fn scan_prefix(txid: Txid) -> Box<[u8]> {
        Box::new(txid_prefix(&txid).prefix)
    }

    pub(crate) fn new(txid: Txid, height: usize) -> Self {
        Self {
            prefix: txid_prefix(&txid),
            height: height.try_into().expect("invalid height"),
        }
    }

    pub(crate) fn to_db_row(&self) -> db::Row {
        serialize(self).into_boxed_slice()
    }

    pub(crate) fn from_db_row(row: &[u8]) -> Self {
        deserialize(&row).expect("bad TxidRow")
    }

    pub(crate) fn height(&self) -> usize {
        self.height.try_into().expect("invalid height")
    }
}

// ***************************************************************************

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct HeaderRow {
    pub(crate) header: BlockHeader,
}

impl_consensus_encoding!(HeaderRow, header);

impl HeaderRow {
    pub(crate) fn new(header: BlockHeader) -> Self {
        Self { header }
    }

    pub(crate) fn to_db_row(&self) -> db::Row {
        serialize(self).into_boxed_slice()
    }

    pub(crate) fn from_db_row(row: &[u8]) -> Self {
        deserialize(&row).expect("bad HeaderRow")
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{spending_prefix, ScriptHash, ScriptHashRow, SpendingPrefix};
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
        let row1 = ScriptHashRow::new(scripthash, 123456);
        let db_row = row1.to_db_row();
        assert_eq!(db_row[..].to_hex(), "a384491d38929fcc40e20100");
        let row2 = ScriptHashRow::from_db_row(&db_row);
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
            SpendingPrefix {
                prefix: [31, 30, 29, 28, 27, 26, 25, 24]
            }
        );
        assert_eq!(
            spending_prefix(OutPoint { txid, vout: 10 }),
            SpendingPrefix {
                prefix: [31, 30, 29, 28, 27, 26, 25, 34]
            }
        );
        assert_eq!(
            spending_prefix(OutPoint { txid, vout: 255 }),
            SpendingPrefix {
                prefix: [31, 30, 29, 28, 27, 26, 26, 23]
            }
        );
        assert_eq!(
            spending_prefix(OutPoint { txid, vout: 256 }),
            SpendingPrefix {
                prefix: [31, 30, 29, 28, 27, 26, 26, 24]
            }
        );
    }
}
