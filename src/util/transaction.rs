use crate::chain::{BlockHash, OutPoint, Transaction, TxIn, TxOut, Txid};
use crate::util::BlockId;

use std::collections::HashMap;

#[cfg(feature = "liquid")]
use bitcoin::hashes::hex::FromHex;

#[cfg(feature = "liquid")]
lazy_static! {
    static ref REGTEST_INITIAL_ISSUANCE_PREVOUT: Txid =
        Txid::from_hex("50cdc410c9d0d61eeacc531f52d2c70af741da33af127c364e52ac1ee7c030a5").unwrap();
}

#[derive(Serialize, Deserialize)]
pub struct TransactionStatus {
    pub confirmed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_height: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_hash: Option<BlockHash>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_time: Option<u32>,
}

impl From<Option<BlockId>> for TransactionStatus {
    fn from(blockid: Option<BlockId>) -> TransactionStatus {
        match blockid {
            Some(b) => TransactionStatus {
                confirmed: true,
                block_height: Some(b.height as usize),
                block_hash: Some(b.hash),
                block_time: Some(b.time),
            },
            None => TransactionStatus {
                confirmed: false,
                block_height: None,
                block_hash: None,
                block_time: None,
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct TxInput {
    pub txid: Txid,
    pub vin: u16,
}

pub fn is_coinbase(txin: &TxIn) -> bool {
    #[cfg(not(feature = "liquid"))]
    return txin.previous_output.is_null();
    #[cfg(feature = "liquid")]
    return txin.is_coinbase();
}

pub fn has_prevout(txin: &TxIn) -> bool {
    #[cfg(not(feature = "liquid"))]
    return !txin.previous_output.is_null();
    #[cfg(feature = "liquid")]
    return !txin.is_coinbase()
        && !txin.is_pegin
        && txin.previous_output.txid != *REGTEST_INITIAL_ISSUANCE_PREVOUT;
}

pub fn is_spendable(txout: &TxOut) -> bool {
    #[cfg(not(feature = "liquid"))]
    return !txout.script_pubkey.is_provably_unspendable();
    #[cfg(feature = "liquid")]
    return !txout.is_fee() && !txout.script_pubkey.is_provably_unspendable();
}

pub fn extract_tx_prevouts<'a>(
    tx: &Transaction,
    txos: &'a HashMap<OutPoint, TxOut>,
    allow_missing: bool,
) -> HashMap<u32, &'a TxOut> {
    tx.input
        .iter()
        .enumerate()
        .filter(|(_, txi)| has_prevout(txi))
        .filter_map(|(index, txi)| {
            Some((
                index as u32,
                txos.get(&txi.previous_output).or_else(|| {
                    assert!(allow_missing, "missing outpoint {:?}", txi.previous_output);
                    None
                })?,
            ))
        })
        .collect()
}
