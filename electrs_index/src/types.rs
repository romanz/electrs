use anyhow::{Context, Result};

use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::PathBuf;

use bitcoin::{
    blockdata::opcodes::all::*,
    consensus::encode::{deserialize, serialize, Decodable, Encodable, ReadExt, VarInt},
    hashes::{borrow_slice_impl, hash_newtype, hex_fmt_impl, index_impl, serde_impl, sha256, Hash},
    util::key::PublicKey,
    BlockHeader, Script, Transaction, Txid,
};

use crate::{daemon::Daemon, db};

hash_newtype!(
    ScriptHash,
    sha256::Hash,
    32,
    doc = "SHA256(scriptPubkey)",
    true
);

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

pub(crate) struct TxUndo {
    pub scripts: Vec<Script>,
}

fn varint_decode<D: std::io::Read>(
    mut d: D,
) -> std::result::Result<usize, bitcoin::consensus::encode::Error> {
    let mut n = 0usize;
    // TODO: add checks
    loop {
        let b = u8::consensus_decode(&mut d)?;
        n = (n << 7) | (b & 0x7F) as usize;
        if b & 0x80 != 0 {
            n += 1;
        } else {
            return Ok(n);
        }
    }
}

fn decode_bytes<D: std::io::Read>(
    mut d: D,
    len: usize,
) -> std::result::Result<Vec<u8>, bitcoin::consensus::encode::Error> {
    let mut ret = vec![0; len];
    d.read_slice(&mut ret)?;
    Ok(ret)
}

const SPECIAL_SCRIPTS: usize = 6;

fn decompress_script(
    script_type: u8,
    mut bytes: Vec<u8>,
) -> std::result::Result<Script, bitcoin::consensus::encode::Error> {
    let builder = bitcoin::blockdata::script::Builder::new();
    let script = match script_type {
        0 => builder
            .push_opcode(OP_DUP)
            .push_opcode(OP_HASH160)
            .push_slice(&bytes[..])
            .push_opcode(OP_EQUALVERIFY)
            .push_opcode(OP_CHECKSIG),
        1 => builder
            .push_opcode(OP_HASH160)
            .push_slice(&bytes[..])
            .push_opcode(OP_EQUAL),
        2 | 3 => {
            bytes.insert(0, script_type);
            builder.push_slice(&bytes).push_opcode(OP_CHECKSIG)
        }
        4 | 5 => {
            bytes.insert(0, script_type - 2);
            let mut pubkey = PublicKey::from_slice(&bytes).expect("bad PublicKey");
            pubkey.compressed = false;
            builder
                .push_slice(&pubkey.to_bytes())
                .push_opcode(OP_CHECKSIG)
        }
        _ => unreachable!(),
    }
    .into_script();
    assert!(script.is_p2pk() || script.is_p2pkh() || script.is_p2sh());
    Ok(script)
}

fn script_decode<D: std::io::Read>(
    mut d: D,
) -> std::result::Result<Script, bitcoin::consensus::encode::Error> {
    let len = varint_decode(&mut d)?;
    // info!("script len={}", len);
    Ok(if len < SPECIAL_SCRIPTS {
        let script_type = len as u8;
        let size = match script_type {
            0 | 1 => 20,
            2 | 3 | 4 | 5 => 32,
            _ => unreachable!(),
        };
        let compressed = decode_bytes(d, size)?;
        decompress_script(script_type, compressed)?
    } else {
        let len = len - SPECIAL_SCRIPTS;
        Script::from(decode_bytes(d, len)?)
    })
}

impl Decodable for TxUndo {
    fn consensus_decode<D: std::io::Read>(
        mut d: D,
    ) -> std::result::Result<Self, bitcoin::consensus::encode::Error> {
        let len = VarInt::consensus_decode(&mut d)?.0;
        let mut scripts = vec![];
        for _ in 0..len {
            let _height_coinbase = varint_decode(&mut d)?;
            assert_eq!(varint_decode(&mut d)?, 0); // unused today
            let _amount = varint_decode(&mut d)?;
            scripts.push(script_decode(&mut d)?)
        }
        Ok(TxUndo { scripts })
    }
}

#[derive(Default)]
pub(crate) struct BlockUndo {
    pub txdata: Vec<TxUndo>,
}

impl Decodable for BlockUndo {
    fn consensus_decode<D: std::io::Read>(
        mut d: D,
    ) -> std::result::Result<Self, bitcoin::consensus::encode::Error> {
        let len = VarInt::consensus_decode(&mut d)?.0;
        let mut txdata = vec![];
        for _ in 0..len {
            txdata.push(TxUndo::consensus_decode(&mut d)?)
        }
        Ok(BlockUndo { txdata })
    }
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
    pub fn reader(&self, daemon: &Daemon) -> Reader {
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

const SCRIPT_HASH_PREFIX_LEN: usize = 8;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct ScriptHashPrefix {
    prefix: [u8; SCRIPT_HASH_PREFIX_LEN],
}

impl_consensus_encoding!(ScriptHashPrefix, prefix);

const TXID_PREFIX_LEN: usize = 8;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct TxidPrefix {
    prefix: [u8; TXID_PREFIX_LEN],
}

impl_consensus_encoding!(TxidPrefix, prefix);

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
    use crate::types::{varint_decode, FilePos, ScriptHash, ScriptHashRow};
    use bitcoin::{hashes::hex::ToHex, Address};
    use serde_json::{from_str, json};
    use std::io::Cursor;
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
    fn test_varint() {
        assert_eq!(varint_decode(Cursor::new(b"\x00")).unwrap(), 0);
        assert_eq!(varint_decode(Cursor::new(b"\x01")).unwrap(), 1);
        assert_eq!(varint_decode(Cursor::new(b"\x10")).unwrap(), 0x10);
        assert_eq!(varint_decode(Cursor::new(b"\x7f")).unwrap(), 0x7f);
        assert_eq!(varint_decode(Cursor::new(b"\x80\x00")).unwrap(), 0x80);
        assert_eq!(varint_decode(Cursor::new(b"\x80\x01")).unwrap(), 0x81);
        assert_eq!(varint_decode(Cursor::new(b"\x80\x7f")).unwrap(), 0xff);
        assert_eq!(varint_decode(Cursor::new(b"\x81\x00")).unwrap(), 0x100);
        assert_eq!(varint_decode(Cursor::new(b"\x81\x23")).unwrap(), 0x123);
        assert_eq!(varint_decode(Cursor::new(b"\x80\x80\x00")).unwrap(), 0x4080);
        assert_eq!(varint_decode(Cursor::new(b"\x81\x86\x07")).unwrap(), 0x8387);
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
