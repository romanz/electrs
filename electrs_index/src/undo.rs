use bitcoin::{
    blockdata::opcodes::all::*,
    consensus::encode::{Decodable, ReadExt, VarInt},
    util::key::PublicKey,
    Script,
};

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

#[cfg(test)]
mod tests {
    use crate::undo::varint_decode;
    use std::io::Cursor;

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
}
