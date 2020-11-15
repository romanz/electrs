use anyhow::{Context, Result};

use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::PathBuf;

use bitcoin::{
    consensus::encode::{deserialize, serialize, Decodable, Encodable},
    hashes::{borrow_slice_impl, hash_newtype, hex_fmt_impl, index_impl, serde_impl, sha256, Hash},
    BlockHeader, Script, Transaction, Txid,
};

use crate::{daemon::Daemon, db};

macro_rules! impl_consensus_encoding {
    ($thing:ident, $($field:ident),+) => (
        impl Encodable for $thing {
            #[inline]
            fn consensus_encode<S: ::std::io::Write>(
                &self,
                mut s: S,
            ) -> Result<usize, bitcoin::consensus::encode::Error> {
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

#[derive(Debug)]
pub struct Confirmed {
    pub tx: Transaction,
    pub txid: Txid,
    pub header: BlockHeader,
    pub height: usize,
    pub file_offset: u32, // for correct ordering (https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-basics.html#status)
}

impl Confirmed {
    pub(crate) fn new(
        tx: Transaction,
        header: BlockHeader,
        height: usize,
        reader: &Reader,
    ) -> Self {
        let txid = tx.txid();
        Self {
            tx,
            txid,
            header,
            height,
            file_offset: reader.offset,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Copy, Clone)]
pub(crate) struct FilePos {
    pub file_id: u16, // currently there are <3k blk files
    pub offset: u32,  // currently blk files are ~128MB (2**27)
}

impl FilePos {
    pub fn reader(self, daemon: &Daemon) -> Reader {
        let file = daemon.blk_file_path(self.file_id as usize);
        Reader {
            file,
            offset: self.offset,
        }
    }
}

impl_consensus_encoding!(FilePos, file_id, offset);

#[derive(Debug)]
pub(crate) struct Reader {
    file: PathBuf,
    offset: u32,
}

impl Reader {
    pub fn read<T>(&self) -> Result<T>
    where
        T: Decodable,
    {
        let mut file =
            File::open(&self.file).with_context(|| format!("failed to open {:?}", self))?;
        file.seek(SeekFrom::Start(self.offset.into()))
            .with_context(|| format!("failed to seek {:?}", self))?;
        T::consensus_decode(&mut file).with_context(|| format!("failed to decode {:?}", self))
    }
}

// ***************************************************************************

hash_newtype!(
    ScriptHash,
    sha256::Hash,
    32,
    doc = "SHA256(scriptPubkey)",
    true
);

impl ScriptHash {
    pub fn new(script: &Script) -> Self {
        ScriptHash::hash(&script[..])
    }

    fn prefix(&self) -> ScriptHashPrefix {
        let mut prefix = [0u8; SCRIPT_HASH_PREFIX_LEN];
        prefix.copy_from_slice(&self.0[..SCRIPT_HASH_PREFIX_LEN]);
        ScriptHashPrefix { prefix }
    }
}

const SCRIPT_HASH_PREFIX_LEN: usize = 8;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct ScriptHashPrefix {
    prefix: [u8; SCRIPT_HASH_PREFIX_LEN],
}

impl_consensus_encoding!(ScriptHashPrefix, prefix);

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct ScriptHashRow {
    prefix: ScriptHashPrefix,
    pos: FilePos, // transaction position on disk
}

impl_consensus_encoding!(ScriptHashRow, prefix, pos);

impl ScriptHashRow {
    pub fn scan_prefix(script_hash: &ScriptHash) -> Box<[u8]> {
        script_hash.0[..SCRIPT_HASH_PREFIX_LEN]
            .to_vec()
            .into_boxed_slice()
    }

    pub fn new(script_hash: ScriptHash, pos: FilePos) -> Self {
        Self {
            prefix: script_hash.prefix(),
            pos,
        }
    }

    pub fn to_db_row(&self) -> db::Row {
        serialize(self).into_boxed_slice()
    }

    pub fn from_db_row(row: &[u8]) -> Result<Self> {
        deserialize(&row).context("bad ScriptHashRow")
    }

    pub fn position(&self) -> &FilePos {
        &self.pos
    }
}

// ***************************************************************************

const TXID_PREFIX_LEN: usize = 8;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct TxidPrefix {
    prefix: [u8; TXID_PREFIX_LEN],
}

impl_consensus_encoding!(TxidPrefix, prefix);

fn txid_prefix(txid: &Txid) -> TxidPrefix {
    let mut prefix = [0u8; TXID_PREFIX_LEN];
    prefix.copy_from_slice(&txid[..TXID_PREFIX_LEN]);
    TxidPrefix { prefix }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct TxidRow {
    prefix: TxidPrefix,
    pos: FilePos, // transaction position on disk
}

impl_consensus_encoding!(TxidRow, prefix, pos);

impl TxidRow {
    pub fn scan_prefix(txid: &Txid) -> Box<[u8]> {
        Box::new(txid_prefix(&txid).prefix)
    }

    pub fn new(txid: Txid, pos: FilePos) -> Self {
        Self {
            prefix: txid_prefix(&txid),
            pos,
        }
    }

    pub fn to_db_row(&self) -> db::Row {
        serialize(self).into_boxed_slice()
    }

    pub fn from_db_row(row: &[u8]) -> Result<Self> {
        deserialize(&row).context("bad TxidRow")
    }

    pub fn position(&self) -> &FilePos {
        &self.pos
    }
}

// ***************************************************************************

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BlockRow {
    pub header: BlockHeader,
    pub pos: FilePos, // block position on disk
    pub size: u32,    // block size on disk
}

impl_consensus_encoding!(BlockRow, header, pos, size);

impl BlockRow {
    pub fn new(header: BlockHeader, pos: FilePos, size: u32) -> Self {
        Self { header, pos, size }
    }

    pub fn to_db_row(&self) -> db::Row {
        serialize(self).into_boxed_slice()
    }

    pub fn from_db_row(row: &[u8]) -> Result<Self> {
        deserialize(&row).context("bad BlockRowKey")
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{FilePos, ScriptHash, ScriptHashRow};
    use bitcoin::{hashes::hex::ToHex, Address};
    use serde_json::{from_str, json};

    use std::str::FromStr;

    #[test]
    fn test_script_hash_serde() {
        let hex = "\"4b3d912c1523ece4615e91bf0d27381ca72169dbf6b1c2ffcc9f92381d4984a3\"";
        let script_hash: ScriptHash = from_str(&hex).unwrap();
        assert_eq!(format!("\"{}\"", script_hash), hex);
        assert_eq!(json!(script_hash).to_string(), hex);
    }

    #[test]
    fn test_script_hash_row() {
        let hex = "\"4b3d912c1523ece4615e91bf0d27381ca72169dbf6b1c2ffcc9f92381d4984a3\"";
        let script_hash: ScriptHash = from_str(&hex).unwrap();
        let row1 = ScriptHashRow::new(
            script_hash,
            FilePos {
                file_id: 0x1234,
                offset: 0x12345678,
            },
        );
        let db_row = row1.to_db_row();
        assert_eq!(db_row[..].to_hex(), "a384491d38929fcc341278563412");
        let row2 = ScriptHashRow::from_db_row(&db_row).unwrap();
        assert_eq!(row1, row2);
    }

    #[test]
    fn test_script_hash() {
        let addr = Address::from_str("1KVNjD3AAnQ3gTMqoTKcWFeqSFujq9gTBT").unwrap();
        let script_hash = ScriptHash::new(&addr.script_pubkey());
        assert_eq!(
            script_hash.to_hex(),
            "00dfb264221d07712a144bda338e89237d1abd2db4086057573895ea2659766a"
        );
    }
}
