use std::collections::HashMap;

use bitcoin::blockdata::script::Instruction::PushBytes;
use bitcoin::consensus::encode::{deserialize, serialize};
use bitcoin::Script;
use bitcoin_hashes::{hex::FromHex, hex::ToHex, sha256, sha256d, Hash};
use elements::confidential::{Asset, Value};
use elements::{AssetIssuance, OutPoint, Proof, Transaction, TxIn, TxOut};
use hex;

use crate::chain::Network;
use crate::errors::*;
use crate::new_index::schema::{
    FundingInfo, SpendingInfo, TxHistoryInfo, TxHistoryKey, TxHistoryRow,
};
use crate::new_index::{parse_hash, DBRow, DB};
use crate::util::{
    full_hash, get_script_asm, has_prevout, is_spendable, script_to_address, AssetId, Bytes,
    FullHash, TxInput,
};

lazy_static! {
    static ref NATIVE_ASSET_ID: sha256d::Hash =
        sha256d::Hash::from_hex("6f0279e9ed041c3d710a9f57d0c02928416460c4b722ae3457a11eec381c526d")
            .unwrap();
    static ref NATIVE_ASSET_ID_TESTNET: sha256d::Hash =
        sha256d::Hash::from_hex("5ac9f65c0efcc4775e0baec4ec03abdde22473cd3cf33c0419ca290e0751b225")
            .unwrap();
}

#[derive(Serialize, Deserialize)]
pub struct BlockProofValue {
    challenge: Script,
    challenge_asm: String,
    solution: Script,
    solution_asm: String,
}

impl From<&Proof> for BlockProofValue {
    fn from(proof: &Proof) -> Self {
        BlockProofValue {
            challenge_asm: get_script_asm(&proof.challenge),
            challenge: proof.challenge.clone(),
            solution_asm: get_script_asm(&proof.solution),
            solution: proof.solution.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct IssuanceValue {
    pub asset_id: Option<String>,
    pub is_reissuance: bool,
    pub asset_blinding_nonce: Option<String>,
    pub asset_entropy: Option<String>,
    pub assetamount: Option<u64>,
    pub assetamountcommitment: Option<String>,
    pub tokenamount: Option<u64>,
    pub tokenamountcommitment: Option<String>,
}

impl From<&TxIn> for IssuanceValue {
    fn from(txin: &TxIn) -> Self {
        let zero = [0u8; 32];

        let issuance = txin.asset_issuance;
        let is_reissuance = issuance.asset_blinding_nonce != zero;
        let asset_id = if is_reissuance {
            None // TODO
        } else {
            get_issuance_assetid(txin).ok().map(|s| s.to_hex())
        };

        IssuanceValue::new(asset_id, &issuance)
    }
}

impl IssuanceValue {
    fn new(asset_id: Option<String>, issuance: &AssetIssuance) -> Self {
        let zero = [0u8; 32];
        let is_reissuance = issuance.asset_blinding_nonce != zero;

        IssuanceValue {
            asset_id,
            is_reissuance,
            asset_blinding_nonce: if is_reissuance {
                Some(hex::encode(issuance.asset_blinding_nonce))
            } else {
                None
            },
            asset_entropy: if issuance.asset_entropy != zero {
                Some(hex::encode(issuance.asset_entropy))
            } else {
                None
            },
            assetamount: match issuance.amount {
                Value::Explicit(value) => Some(value),
                _ => None,
            },
            assetamountcommitment: match issuance.amount {
                Value::Confidential(..) => Some(hex::encode(serialize(&issuance.amount))),
                _ => None,
            },
            tokenamount: match issuance.inflation_keys {
                Value::Explicit(value) => Some(value),
                _ => None,
            },
            tokenamountcommitment: match issuance.inflation_keys {
                Value::Confidential(..) => Some(hex::encode(serialize(&issuance.inflation_keys))),
                _ => None,
            },
        }
    }
}

// DB representation
#[derive(Serialize, Deserialize, Debug)]
pub struct AssetRowValue {
    pub issuance_txid: FullHash,
    pub issuance_vin: u16,
    pub prev_txid: FullHash,
    pub prev_vout: u16,
    pub issuance: Bytes, // bincode does not like dealing with AssetIssuance, deserialization fails with "invalid type: sequence, expected a struct"
}

// Internal API representation
#[derive(Serialize, Deserialize)]
pub struct AssetEntry {
    pub asset_id: sha256d::Hash,
    pub issuance_txin: TxInput,
    pub issuance_prevout: OutPoint,
    pub issuance: AssetIssuance,
}

// JSON representation for external HTTP API
#[derive(Serialize, Deserialize)]
pub struct AssetValue {
    pub asset_id: sha256d::Hash,
    pub issuance_txin: TxInput,
    pub issuance_prevout: OutPoint,
    pub issuance: IssuanceValue,
}

impl From<AssetEntry> for AssetValue {
    fn from(entry: AssetEntry) -> Self {
        let AssetEntry {
            asset_id,
            issuance_txin,
            issuance_prevout,
            issuance,
        } = entry;
        Self {
            asset_id,
            issuance_txin,
            issuance_prevout,
            issuance: IssuanceValue::new(Some(asset_id.to_hex()), &issuance),
        }
    }
}

impl AssetEntry {
    pub fn from_row(asset_hash: &[u8], asset: &AssetRowValue) -> Self {
        Self {
            asset_id: parse_hash(&full_hash(&asset_hash[..])),
            issuance_txin: TxInput {
                txid: parse_hash(&asset.issuance_txid),
                vin: asset.issuance_vin,
            },
            issuance_prevout: OutPoint {
                txid: parse_hash(&asset.prev_txid),
                vout: asset.prev_vout as u32,
            },
            issuance: deserialize(&asset.issuance).expect("failed parsing AssetIssuance"),
        }
    }
}

fn get_issuance_assetid(txin: &TxIn) -> Result<AssetId> {
    if !txin.has_issuance {
        bail!("input has no issuance");
    }

    let contract_hash = sha256::Hash::from_slice(&txin.asset_issuance.asset_entropy)
        .chain_err(|| "invalid entropy")?;
    let entropy = AssetId::generate_asset_entropy(txin.previous_output.clone(), contract_hash);

    Ok(AssetId::from_entropy(entropy))
}

#[derive(Serialize, Deserialize, Debug)]
pub struct IssuanceInfo {
    pub txid: FullHash,
    pub vin: u16,
}

// TODO: index mempool transactions
pub fn index_elements_transaction(
    tx: &Transaction,
    confirmed_height: u32,
    previous_txos_map: &HashMap<OutPoint, TxOut>,
    rows: &mut Vec<DBRow>,
) {
    // persist asset and its history index:
    //      i{asset-id} → {issuance-txid:vin}{prev-txid:vout}{issuance}
    //      I{asset-id}{issuance-height}I{issuance-txid:vin} → ""
    //      I{asset-id}{funding-height}F{funding-txid:vout}{value} → ""
    //      I{asset-id}{spending-height}S{spending-txid:vin}{funding-txid:vout}{value} → ""
    let txid = full_hash(&tx.txid()[..]);
    for (txo_index, txo) in tx.output.iter().enumerate() {
        if !is_spendable(txo) || !is_issued_asset(&txo.asset) {
            continue;
        }
        let history = asset_history_row(
            &txo.asset,
            confirmed_height,
            TxHistoryInfo::Funding(FundingInfo {
                txid,
                vout: txo_index as u16,
                value: txo.value,
            }),
        );
        rows.push(history.to_row())
    }

    for (txi_index, txi) in tx.input.iter().enumerate() {
        if !has_prevout(txi) {
            continue;
        }
        let prev_txo = previous_txos_map
            .get(&txi.previous_output)
            .expect(&format!("missing previous txo {}", txi.previous_output));

        if is_issued_asset(&prev_txo.asset) {
            let history = asset_history_row(
                &prev_txo.asset,
                confirmed_height,
                TxHistoryInfo::Spending(SpendingInfo {
                    txid,
                    vin: txi_index as u16,
                    prev_txid: full_hash(&txi.previous_output.txid[..]),
                    prev_vout: txi.previous_output.vout as u16,
                    value: prev_txo.value,
                }),
            );
            rows.push(history.to_row());
        }

        if txi.has_issuance() {
            let asset_id = match get_issuance_assetid(txi) {
                Err(e) => {
                    warn!("skipping issuance due to error: {:?}", e);
                    continue;
                }
                Ok(asset_id) => asset_id,
            };
            let asset_hash = asset_id.into_inner().into_inner();
            let asset = Asset::Explicit(sha256d::Hash::from_inner(asset_hash.clone()));

            // the issuance is kept twice: once in the history index under I<asset><height><txid:vin>,
            // and once separately under i<asset> for asset lookup with some more associated metadata

            let history = asset_history_row(
                &asset,
                confirmed_height,
                TxHistoryInfo::Issuance(IssuanceInfo {
                    txid,
                    vin: txi_index as u16,
                }),
            );
            rows.push(history.to_row());

            let asset_row = AssetRowValue {
                issuance_txid: txid,
                issuance_vin: txi_index as u16,
                prev_txid: full_hash(&txi.previous_output.txid[..]),
                prev_vout: txi.previous_output.vout as u16,
                issuance: serialize(&txi.asset_issuance),
            };
            rows.push(DBRow {
                key: [b"i", &asset_hash[..]].concat(),
                value: bincode::serialize(&asset_row).unwrap(),
            });
        }
    }
}

fn is_issued_asset(asset: &Asset) -> bool {
    match asset {
        Asset::Null | Asset::Confidential(..) => false,
        Asset::Explicit(asset_hash) => {
            asset_hash != &*NATIVE_ASSET_ID && asset_hash != &*NATIVE_ASSET_ID_TESTNET
        }
    }
}

fn asset_history_row(asset: &Asset, confirmed_height: u32, txinfo: TxHistoryInfo) -> TxHistoryRow {
    if let Asset::Explicit(asset_hash) = asset {
        let key = TxHistoryKey {
            code: b'I',
            hash: full_hash(&asset_hash[..]),
            confirmed_height,
            txinfo,
        };
        TxHistoryRow { key }
    } else {
        unreachable!();
    }
}

pub fn lookup_asset(history_db: &DB, asset_hash: &[u8]) -> Option<AssetEntry> {
    history_db
        .get(&[b"i", &asset_hash[..]].concat())
        .map(|val| bincode::deserialize(&val).expect("failed to parse AssetRowValue"))
        .map(|row_val| AssetEntry::from_row(asset_hash, &row_val))
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PegOutRequest {
    pub genesis_hash: String,
    pub scriptpubkey: Script,
    pub scriptpubkey_asm: String,
    pub scriptpubkey_address: Option<String>,
}

impl PegOutRequest {
    pub fn parse(
        script: &Script,
        parent_network: &Network,
        parent_genesis_hash: &str,
    ) -> Option<PegOutRequest> {
        if !script.is_op_return() {
            return None;
        }

        let nulldata: Vec<_> = script.iter(true).skip(1).collect();
        if nulldata.len() < 2 {
            return None;
        }

        let genesis_hash = if let PushBytes(data) = nulldata[0] {
            let mut data = data.to_vec();
            data.reverse();
            hex::encode(data)
        } else {
            return None;
        };

        let scriptpubkey = if let PushBytes(data) = nulldata[1] {
            Script::from(data.to_vec())
        } else {
            return None;
        };

        if genesis_hash != parent_genesis_hash {
            return None;
        }

        let scriptpubkey_asm = get_script_asm(&scriptpubkey);
        let scriptpubkey_address = script_to_address(&scriptpubkey, parent_network);

        Some(PegOutRequest {
            genesis_hash,
            scriptpubkey,
            scriptpubkey_asm,
            scriptpubkey_address,
        })
    }
}
