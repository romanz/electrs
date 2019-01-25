use elements::{Proof, AssetIssuance};
use elements::confidential::Value;
use bitcoin::Script;
use bitcoin::consensus::encode::serialize;

use hex;

use crate::util::get_script_asm;

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
    pub is_reissuance: bool,
    pub asset_blinding_nonce: Option<String>,
    pub asset_entropy: Option<String>,
    pub assetamount: Option<u64>,
    pub assetamountcommitment: Option<String>,
    pub tokenamount: Option<u64>,
    pub tokenamountcommitment: Option<String>,
}

impl From<&AssetIssuance> for IssuanceValue {
    fn from(issuance: &AssetIssuance) -> Self {
        let zero = [0u8;32];
        let is_reissuance = issuance.asset_blinding_nonce != zero;

        IssuanceValue {
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
                Value::Explicit(value) => Some(value / 100000000), // https://github.com/ElementsProject/rust-elements/issues/7
                _ => None,
            },
            tokenamountcommitment: match issuance.inflation_keys {
                Value::Confidential(..) => {
                    Some(hex::encode(serialize(&issuance.inflation_keys)))
                }
                _ => None,
            },
        }
    }
}
