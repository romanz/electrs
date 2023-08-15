use bitcoin::hashes::{sha256, Hash};
use elements::secp256k1_zkp::ZERO_TWEAK;
use elements::{confidential::Value, encode::serialize, issuance::ContractHash, AssetId, TxIn};

pub mod asset;
pub mod peg;
mod registry;

use asset::get_issuance_entropy;
pub use asset::{lookup_asset, LiquidAsset};
pub use registry::{AssetRegistry, AssetSorting};

#[derive(Serialize, Deserialize, Clone)]
pub struct IssuanceValue {
    pub asset_id: AssetId,
    pub is_reissuance: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_blinding_nonce: Option<elements::secp256k1_zkp::Tweak>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_hash: Option<ContractHash>,
    pub asset_entropy: sha256::Midstate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assetamount: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assetamountcommitment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokenamount: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokenamountcommitment: Option<String>,
}

impl From<&TxIn> for IssuanceValue {
    fn from(txin: &TxIn) -> Self {
        let issuance = &txin.asset_issuance;
        let is_reissuance = issuance.asset_blinding_nonce != ZERO_TWEAK;

        let asset_entropy = get_issuance_entropy(txin).expect("invalid issuance");
        let asset_id = AssetId::from_entropy(asset_entropy);

        let contract_hash = if !is_reissuance {
            Some(ContractHash::from_slice(&issuance.asset_entropy).expect("invalid asset entropy"))
        } else {
            None
        };

        IssuanceValue {
            asset_id,
            asset_entropy,
            contract_hash,
            is_reissuance,
            asset_blinding_nonce: if is_reissuance {
                Some(issuance.asset_blinding_nonce)
            } else {
                None
            },
            assetamount: match issuance.amount {
                Value::Explicit(value) => Some(value),
                Value::Null => Some(0),
                Value::Confidential(..) => None,
            },
            assetamountcommitment: match issuance.amount {
                Value::Confidential(..) => Some(hex::encode(serialize(&issuance.amount))),
                _ => None,
            },
            tokenamount: match issuance.inflation_keys {
                Value::Explicit(value) => Some(value),
                Value::Null => Some(0),
                Value::Confidential(..) => None,
            },
            tokenamountcommitment: match issuance.inflation_keys {
                Value::Confidential(..) => Some(hex::encode(serialize(&issuance.inflation_keys))),
                _ => None,
            },
        }
    }
}
