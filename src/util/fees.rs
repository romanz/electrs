#[cfg(feature = "liquid")]
use crate::chain::Value;
use crate::chain::{Transaction, TxOut};
use std::collections::HashMap;

const VSIZE_BIN_WIDTH: u32 = 50_000; // in vbytes

pub struct TxFeeInfo {
    pub fee: u64,   // in satoshis
    pub vsize: u32, // in virtual bytes (= weight/4)
    pub fee_per_vbyte: f32,
}

impl TxFeeInfo {
    #[cfg(not(feature = "liquid"))]
    pub fn new(tx: &Transaction, prevouts: &HashMap<u32, &TxOut>) -> Self {
        let total_in: u64 = prevouts.values().map(|prevout| prevout.value).sum();
        let total_out: u64 = tx.output.iter().map(|vout| vout.value).sum();
        let fee = total_in - total_out;
        let vsize = tx.get_weight() / 4;

        TxFeeInfo {
            fee,
            vsize: vsize as u32,
            fee_per_vbyte: fee as f32 / vsize as f32,
        }
    }

    #[cfg(feature = "liquid")]
    pub fn new(tx: &Transaction, _prevouts: &HashMap<u32, &TxOut>) -> Self {
        let fee = tx
            .output
            .iter()
            .find(|vout| vout.is_fee())
            .map_or(0, |vout| match vout.value {
                Value::Explicit(value) => value,
                _ => 0u64,
            });

        let vsize = tx.get_weight() / 4;

        TxFeeInfo {
            fee,
            vsize: vsize as u32,
            fee_per_vbyte: fee as f32 / vsize as f32,
        }
    }
}

pub fn make_fee_histogram(mut entries: Vec<&TxFeeInfo>) -> Vec<(f32, u32)> {
    entries.sort_unstable_by(|e1, e2| e1.fee_per_vbyte.partial_cmp(&e2.fee_per_vbyte).unwrap());

    let mut histogram = vec![];
    let mut bin_size = 0;
    let mut last_fee_rate = None;
    for e in entries.iter().rev() {
        bin_size += e.vsize;
        if bin_size > VSIZE_BIN_WIDTH && last_fee_rate.map_or(true, |last| e.fee_per_vbyte < last) {
            // vsize of transactions paying >= e.fee_per_vbyte
            histogram.push((e.fee_per_vbyte, bin_size));
            bin_size = 0;
        }
        last_fee_rate = Some(e.fee_per_vbyte);
    }
    if let Some(fee_rate) = last_fee_rate {
        histogram.push((fee_rate, bin_size));
    }
    histogram
}
