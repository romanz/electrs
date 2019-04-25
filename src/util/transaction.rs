use bitcoin_hashes::sha256d::Hash as Sha256dHash;
#[cfg(feature = "liquid")]
use bitcoin_hashes::hex::ToHex;

use crate::chain::{TxIn, TxOut};
use crate::util::BlockId;

#[cfg(feature = "liquid")]
const REGTEST_INITIAL_ISSUANCE_PREVOUT: &str =
    "50cdc410c9d0d61eeacc531f52d2c70af741da33af127c364e52ac1ee7c030a5";

#[derive(Serialize, Deserialize)]
pub struct TransactionStatus {
    pub confirmed: bool,
    pub block_height: Option<usize>,
    pub block_hash: Option<Sha256dHash>,
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
        && txin.previous_output.txid.to_hex() != REGTEST_INITIAL_ISSUANCE_PREVOUT;
}

pub fn is_spendable(txout: &TxOut) -> bool {
    #[cfg(not(feature = "liquid"))]
    return !txout.script_pubkey.is_provably_unspendable();
    #[cfg(feature = "liquid")]
    return !txout.is_fee() && !txout.script_pubkey.is_provably_unspendable();
}
