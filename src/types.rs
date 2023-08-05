use anyhow::Result;

use std::convert::TryFrom;

use bitcoin::blockdata::block::Header as BlockHeader;
use bitcoin::{
    consensus::encode::{deserialize, Decodable, Encodable},
    hashes::{hash_newtype, sha256, Hash},
    OutPoint, Script, Txid,
};
use bitcoin_slices::bsl;

use crate::db;

macro_rules! impl_consensus_encoding {
    ($thing:ident, $($field:ident),+) => (
        impl Encodable for $thing {
            #[inline]
            fn consensus_encode<S: ::std::io::Write + ?Sized>(
                &self,
                s: &mut S,
            ) -> Result<usize, std::io::Error> {
                let mut len = 0;
                $(len += self.$field.consensus_encode(s)?;)+
                Ok(len)
            }
        }

        impl Decodable for $thing {
            #[inline]
            fn consensus_decode<D: ::std::io::Read + ?Sized>(
                d: &mut D,
            ) -> Result<$thing, bitcoin::consensus::encode::Error> {
                Ok($thing {
                    $($field: Decodable::consensus_decode(d)?),+
                })
            }
        }
    );
}

const HASH_PREFIX_LEN: usize = 8;
const HEIGHT_SIZE: usize = 4;

type HashPrefix = [u8; HASH_PREFIX_LEN];
type Height = u32;
pub(crate) type SerBlock = Vec<u8>;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct HashPrefixRow {
    prefix: [u8; HASH_PREFIX_LEN],
    height: Height, // transaction confirmed height
}

const HASH_PREFIX_ROW_SIZE: usize = HASH_PREFIX_LEN + HEIGHT_SIZE;

impl HashPrefixRow {
    pub(crate) fn to_db_row(&self) -> db::Row {
        let mut vec = Vec::with_capacity(HASH_PREFIX_ROW_SIZE);
        let len = self
            .consensus_encode(&mut vec)
            .expect("in-memory writers don't error");
        debug_assert_eq!(len, HASH_PREFIX_ROW_SIZE);
        vec.into_boxed_slice()
    }

    pub(crate) fn from_db_row(row: &[u8]) -> Self {
        deserialize(row).expect("bad HashPrefixRow")
    }

    pub fn height(&self) -> usize {
        usize::try_from(self.height).expect("invalid height")
    }
}

impl_consensus_encoding!(HashPrefixRow, prefix, height);

hash_newtype! {
    /// https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-basics.html#script-hashes
    #[hash_newtype(backward)]
    pub struct ScriptHash(sha256::Hash);
}

impl ScriptHash {
    pub fn new(script: &Script) -> Self {
        ScriptHash::hash(script.as_bytes())
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

    pub(crate) fn row(scripthash: ScriptHash, height: usize) -> HashPrefixRow {
        HashPrefixRow {
            prefix: scripthash.prefix(),
            height: Height::try_from(height).expect("invalid height"),
        }
    }
}

// ***************************************************************************

hash_newtype! {
    /// https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-basics.html#status
    pub struct StatusHash(sha256::Hash);
}

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

    pub(crate) fn row(outpoint: OutPoint, height: usize) -> HashPrefixRow {
        HashPrefixRow {
            prefix: spending_prefix(outpoint),
            height: Height::try_from(height).expect("invalid height"),
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

    pub(crate) fn row(txid: Txid, height: usize) -> HashPrefixRow {
        HashPrefixRow {
            prefix: txid_prefix(&txid),
            height: Height::try_from(height).expect("invalid height"),
        }
    }
}

// ***************************************************************************

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct HeaderRow {
    pub(crate) header: BlockHeader,
}

const HEADER_ROW_SIZE: usize = 80;

impl_consensus_encoding!(HeaderRow, header);

impl HeaderRow {
    pub(crate) fn new(header: BlockHeader) -> Self {
        Self { header }
    }

    pub(crate) fn to_db_row(&self) -> db::Row {
        let mut vec = Vec::with_capacity(HEADER_ROW_SIZE);
        let len = self
            .consensus_encode(&mut vec)
            .expect("in-memory writers don't error");
        debug_assert_eq!(len, HEADER_ROW_SIZE);
        vec.into_boxed_slice()
    }

    pub(crate) fn from_db_row(row: &[u8]) -> Self {
        deserialize(row).expect("bad HeaderRow")
    }
}

pub(crate) fn bsl_txid(tx: &bsl::Transaction) -> Txid {
    bitcoin::Txid::from_slice(tx.txid_sha2().as_slice()).expect("invalid txid")
}

#[cfg(test)]
mod tests {
    use crate::types::{spending_prefix, HashPrefixRow, ScriptHash, ScriptHashRow, TxidRow};
    use bitcoin::{Address, OutPoint, Txid};
    use hex_lit::hex;
    use serde_json::{from_str, json};

    use std::str::FromStr;

    #[test]
    fn test_scripthash_serde() {
        let hex = "\"4b3d912c1523ece4615e91bf0d27381ca72169dbf6b1c2ffcc9f92381d4984a3\"";
        let scripthash: ScriptHash = from_str(hex).unwrap();
        assert_eq!(format!("\"{}\"", scripthash), hex);
        assert_eq!(json!(scripthash).to_string(), hex);
    }

    #[test]
    fn test_scripthash_row() {
        let hex = "\"4b3d912c1523ece4615e91bf0d27381ca72169dbf6b1c2ffcc9f92381d4984a3\"";
        let scripthash: ScriptHash = from_str(hex).unwrap();
        let row1 = ScriptHashRow::row(scripthash, 123456);
        let db_row = row1.to_db_row();
        assert_eq!(&*db_row, &hex!("a384491d38929fcc40e20100"));
        let row2 = HashPrefixRow::from_db_row(&db_row);
        assert_eq!(row1, row2);
    }

    #[test]
    fn test_scripthash() {
        let addr = Address::from_str("1KVNjD3AAnQ3gTMqoTKcWFeqSFujq9gTBT")
            .unwrap()
            .assume_checked();
        let scripthash = ScriptHash::new(&addr.script_pubkey());
        assert_eq!(
            scripthash,
            "00dfb264221d07712a144bda338e89237d1abd2db4086057573895ea2659766a"
                .parse()
                .unwrap()
        );
    }

    #[test]
    fn test_txid1_prefix() {
        // duplicate txids from BIP-30
        let hex = "d5d27987d2a3dfc724e359870c6644b40e497bdc0589a033220fe15429d88599";
        let txid = Txid::from_str(hex).unwrap();

        let row1 = TxidRow::row(txid, 91812);
        let row2 = TxidRow::row(txid, 91842);

        assert_eq!(&*row1.to_db_row(), &hex!("9985d82954e10f22a4660100"));
        assert_eq!(&*row2.to_db_row(), &hex!("9985d82954e10f22c2660100"));
    }

    #[test]
    fn test_txid2_prefix() {
        // duplicate txids from BIP-30
        let hex = "e3bf3d07d4b0375638d5f1db5255fe07ba2c4cb067cd81b84ee974b6585fb468";
        let txid = Txid::from_str(hex).unwrap();

        let row1 = TxidRow::row(txid, 91722);
        let row2 = TxidRow::row(txid, 91880);

        // low-endian encoding => rows should be sorted according to block height
        assert_eq!(&*row1.to_db_row(), &hex!("68b45f58b674e94e4a660100"));
        assert_eq!(&*row2.to_db_row(), &hex!("68b45f58b674e94ee8660100"));
    }

    #[test]
    fn test_spending_prefix() {
        let txid = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
            .parse()
            .unwrap();

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
