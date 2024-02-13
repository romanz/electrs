use crate::chain::{Network, Transaction, TxOut};
use std::collections::HashMap;

const VSIZE_BIN_WIDTH: u64 = 50_000; // in vbytes

pub struct TxFeeInfo {
    pub fee: u64,           // in satoshis
    pub vsize: u64,         // in virtual bytes (= weight/4)
    pub fee_per_vbyte: f64, // in sat/vb
}

impl TxFeeInfo {
    pub fn new(tx: &Transaction, prevouts: &HashMap<u32, &TxOut>, network: Network) -> Self {
        let fee = get_tx_fee(tx, prevouts, network);

        let weight = tx.weight();
        #[cfg(not(feature = "liquid"))] // rust-bitcoin has a wrapper Weight type
        let weight = weight.to_wu();

        let vsize_float = weight as f64 / 4f64; // for more accurate sat/vB

        TxFeeInfo {
            fee,
            vsize: vsize_float.ceil() as u64,
            fee_per_vbyte: fee as f64 / vsize_float,
        }
    }
}

#[cfg(not(feature = "liquid"))]
pub fn get_tx_fee(tx: &Transaction, prevouts: &HashMap<u32, &TxOut>, _network: Network) -> u64 {
    if tx.is_coinbase() {
        return 0;
    }

    let total_in: u64 = prevouts
        .values()
        .map(|prevout| prevout.value.to_sat())
        .sum();
    let total_out: u64 = tx.output.iter().map(|vout| vout.value.to_sat()).sum();
    total_in - total_out
}

#[cfg(feature = "liquid")]
pub fn get_tx_fee(tx: &Transaction, _prevouts: &HashMap<u32, &TxOut>, network: Network) -> u64 {
    tx.fee_in(*network.native_asset())
}

pub fn make_fee_histogram(mut entries: Vec<&TxFeeInfo>) -> Vec<(f64, u64)> {
    entries.sort_unstable_by(|e1, e2| e1.fee_per_vbyte.partial_cmp(&e2.fee_per_vbyte).unwrap());

    let mut histogram = vec![];
    let mut bin_size = 0;
    let mut last_fee_rate = 0.0;
    for e in entries.iter().rev() {
        if bin_size > VSIZE_BIN_WIDTH && last_fee_rate != e.fee_per_vbyte {
            // vsize of transactions paying >= last_fee_rate
            histogram.push((last_fee_rate, bin_size));
            bin_size = 0;
        }
        last_fee_rate = e.fee_per_vbyte;
        bin_size += e.vsize;
    }
    if bin_size > 0 {
        histogram.push((last_fee_rate, bin_size));
    }
    histogram
}
