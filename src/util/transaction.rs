use crate::chain::{BlockHash, OutPoint, Transaction, TxIn, TxOut, Txid};
use crate::util::BlockId;

use std::collections::{BTreeSet, HashMap};

#[cfg(feature = "liquid")]
lazy_static! {
    static ref REGTEST_INITIAL_ISSUANCE_PREVOUT: Txid =
        "50cdc410c9d0d61eeacc531f52d2c70af741da33af127c364e52ac1ee7c030a5"
            .parse()
            .unwrap();
    static ref TESTNET_INITIAL_ISSUANCE_PREVOUT: Txid =
        "0c52d2526a5c9f00e9fb74afd15dd3caaf17c823159a514f929ae25193a43a52"
            .parse()
            .unwrap();
}

#[derive(Serialize, Deserialize, Debug)]
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

#[cfg(feature = "liquid")]
pub fn optional_value_for_newer_blocks(
    block_id: Option<BlockId>,
    check_time: u32,
    value: usize,
) -> Option<usize> {
    match block_id {
        // use the provided value only if it was after the "activation" time
        Some(b) if b.time >= check_time => Some(value),
        // otherwise don't include it
        Some(_) => None,
        // also use the value for unconfirmed blocks
        None => Some(value),
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
        && txin.previous_output.txid != *REGTEST_INITIAL_ISSUANCE_PREVOUT
        && txin.previous_output.txid != *TESTNET_INITIAL_ISSUANCE_PREVOUT;
}

pub fn is_spendable(txout: &TxOut) -> bool {
    #[cfg(not(feature = "liquid"))]
    return !txout.script_pubkey.is_op_return();
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

pub fn get_prev_outpoints<'a>(txs: impl Iterator<Item = &'a Transaction>) -> BTreeSet<OutPoint> {
    txs.flat_map(|tx| {
        tx.input
            .iter()
            .filter(|txin| has_prevout(txin))
            .map(|txin| txin.previous_output)
    })
    .collect()
}

pub fn serialize_outpoint<S>(outpoint: &OutPoint, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::ser::Serializer,
{
    use serde::ser::SerializeStruct;
    let mut s = serializer.serialize_struct("OutPoint", 2)?;
    s.serialize_field("txid", &outpoint.txid)?;
    s.serialize_field("vout", &outpoint.vout)?;
    s.end()
}

#[cfg(all(test, feature = "liquid"))]
mod test {
    use super::optional_value_for_newer_blocks;
    use crate::util::BlockId;
    use bitcoin::hashes::Hash;
    use elements::BlockHash;

    #[test]
    fn opt_value_newer_block() {
        let value = 123;
        let check_time = 32;
        let hash = BlockHash::from_slice(&[0; 32]).unwrap();
        let height = 456;

        // unconfirmed block should include the value
        let block_id = None;
        assert_eq!(
            optional_value_for_newer_blocks(block_id, check_time, value),
            Some(value)
        );

        // block time before check_time should NOT include the value
        let block_id = Some(BlockId {
            height,
            hash,
            time: 0,
        });
        assert_eq!(
            optional_value_for_newer_blocks(block_id, check_time, value),
            None
        );
        let block_id = Some(BlockId {
            height,
            hash,
            time: 31,
        });
        assert_eq!(
            optional_value_for_newer_blocks(block_id, check_time, value),
            None
        );

        // block time on or after check_time should include the value
        let block_id = Some(BlockId {
            height,
            hash,
            time: 32,
        });
        assert_eq!(
            optional_value_for_newer_blocks(block_id, check_time, value),
            Some(value)
        );
        let block_id = Some(BlockId {
            height,
            hash,
            time: 33,
        });
        assert_eq!(
            optional_value_for_newer_blocks(block_id, check_time, value),
            Some(value)
        );
        let block_id = Some(BlockId {
            height,
            hash,
            time: 333,
        });
        assert_eq!(
            optional_value_for_newer_blocks(block_id, check_time, value),
            Some(value)
        );
    }
}
