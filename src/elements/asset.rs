use std::collections::{HashMap, HashSet};

use bitcoin::consensus::encode::{deserialize, serialize};
use bitcoin_hashes::{hex::FromHex, sha256, sha256d, Hash};
use elements::confidential::{Asset, Value};
use elements::{AssetIssuance, OutPoint, Transaction, TxIn, TxOut};

use crate::errors::*;
use crate::new_index::schema::{
    FundingInfo, SpendingInfo, TxHistoryInfo, TxHistoryKey, TxHistoryRow,
};
use crate::new_index::{db::DBFlush, parse_hash, ChainQuery, DBRow};
use crate::util::{full_hash, has_prevout, is_spendable, Bytes, FullHash, TxInput};

use crate::elements::{
    registry::{AssetMeta, AssetRegistry},
    AssetId,
};

lazy_static! {
    static ref NATIVE_ASSET_ID: sha256d::Hash =
        sha256d::Hash::from_hex("6f0279e9ed041c3d710a9f57d0c02928416460c4b722ae3457a11eec381c526d")
            .unwrap();
    static ref NATIVE_ASSET_ID_TESTNET: sha256d::Hash =
        sha256d::Hash::from_hex("5ac9f65c0efcc4775e0baec4ec03abdde22473cd3cf33c0419ca290e0751b225")
            .unwrap();
}

#[derive(Serialize)]
pub struct AssetEntry {
    pub asset_id: sha256d::Hash, // not really a sha256d
    pub issuance_txin: TxInput,
    pub issuance_prevout: OutPoint,
    pub reissuance_token: sha256d::Hash, // not really a sha256d

    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_hash: Option<sha256d::Hash>, // not really a sha256d

    pub chain_stats: AssetStats,

    // optional metadata from registry
    #[serde(flatten)]
    pub meta: Option<AssetMeta>,
}

// DB representation
#[derive(Serialize, Deserialize, Debug)]
pub struct AssetRow {
    pub issuance_txid: FullHash,
    pub issuance_vin: u16,
    pub prev_txid: FullHash,
    pub prev_vout: u16,
    pub issuance: Bytes, // bincode does not like dealing with AssetIssuance, deserialization fails with "invalid type: sequence, expected a struct"
    pub reissuance_token: FullHash,
}

impl AssetEntry {
    pub fn new(
        asset_hash: &[u8],
        asset: AssetRow,
        chain_stats: AssetStats,
        meta: Option<AssetMeta>,
    ) -> Self {
        let issuance: AssetIssuance =
            deserialize(&asset.issuance).expect("failed parsing AssetIssuance");

        // XXX this isn't really a double-hash, sha256d is only being used to get backward
        // serialization that matches the one used by elements-cpp
        let reissuance_token = sha256d::Hash::from_inner(asset.reissuance_token);
        let contract_hash = if issuance.asset_entropy != [0u8; 32] {
            Some(sha256d::Hash::from_inner(issuance.asset_entropy))
        } else {
            None
        };

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
            contract_hash,
            reissuance_token,
            chain_stats,
            meta,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct IssuingInfo {
    pub txid: FullHash,
    pub vin: u16,
    pub is_reissuance: bool,
    // None for blinded issuances
    pub issued_amount: Option<u64>,
    pub token_amount: Option<u64>,
}

// Index confirmed transaction and write histoy entries as db rows into `rows`
pub fn index_confirmed_tx_assets(
    tx: &Transaction,
    confirmed_height: u32,
    previous_txos_map: &HashMap<OutPoint, TxOut>,
    rows: &mut Vec<DBRow>,
) {
    let (history, issuances) = index_tx_assets(tx, previous_txos_map);

    rows.extend(
        history
            .into_iter()
            .map(|(asset_id, info)| asset_history_row(&asset_id, confirmed_height, info).to_row()),
    );

    // the initial issuance is kept twice: once in the history index under I<asset><height><txid:vin>,
    // and once separately under i<asset> for asset lookup with some more associated metadata.
    // reissuances are only kept under the history index.
    rows.extend(issuances.into_iter().map(|(asset_id, asset_row)| DBRow {
        key: [b"i", &asset_id[..]].concat(),
        value: bincode::serialize(&asset_row).unwrap(),
    }));
}

// Internal utility function, index atransaction and return its history entries and issuances
fn index_tx_assets(
    tx: &Transaction,
    previous_txos_map: &HashMap<OutPoint, TxOut>,
) -> (
    Vec<(sha256d::Hash, TxHistoryInfo)>,
    Vec<(sha256d::Hash, AssetRow)>,
) {
    let mut history = vec![];
    let mut issuances = vec![];

    let txid = full_hash(&tx.txid()[..]);
    for (txo_index, txo) in tx.output.iter().enumerate() {
        if let Some(asset_id) = get_user_asset_id(&txo.asset) {
            let funding_info = FundingInfo {
                txid,
                vout: txo_index as u16,
                value: txo.value,
            };

            history.push((
                asset_id,
                if is_spendable(txo) {
                    TxHistoryInfo::Funding(funding_info)
                } else {
                    TxHistoryInfo::Burning(funding_info)
                },
            ));
        }
    }

    for (txi_index, txi) in tx.input.iter().enumerate() {
        if !has_prevout(txi) {
            continue;
        }
        let prev_txo = previous_txos_map
            .get(&txi.previous_output)
            .expect(&format!("missing previous txo {}", txi.previous_output));

        if let Some(asset_id) = get_user_asset_id(&prev_txo.asset) {
            history.push((
                asset_id,
                TxHistoryInfo::Spending(SpendingInfo {
                    txid,
                    vin: txi_index as u16,
                    prev_txid: full_hash(&txi.previous_output.txid[..]),
                    prev_vout: txi.previous_output.vout as u16,
                    value: prev_txo.value,
                }),
            ));
        }

        if txi.has_issuance() {
            let is_reissuance = txi.asset_issuance.asset_blinding_nonce != [0u8; 32];

            let asset_entropy = get_issuance_entropy(txi).expect("invalid issuance");
            let asset_id = AssetId::from_entropy(asset_entropy.clone());
            // ugh, should eventually switch to using AssetIds everywhere
            let asset_id = sha256d::Hash::from_inner(asset_id.into_inner().into_inner());

            let issued_amount = match txi.asset_issuance.amount {
                Value::Explicit(amount) => Some(amount),
                _ => None,
            };
            let token_amount = match txi.asset_issuance.inflation_keys {
                Value::Explicit(amount) => Some(amount),
                Value::Null => Some(0),
                _ => None,
            };

            history.push((
                asset_id,
                TxHistoryInfo::Issuing(IssuingInfo {
                    txid,
                    vin: txi_index as u16,
                    is_reissuance,
                    issued_amount,
                    token_amount,
                }),
            ));

            if !is_reissuance {
                let is_confidential = match txi.asset_issuance.inflation_keys {
                    Value::Confidential(..) => true,
                    _ => false,
                };
                let reissuance_token =
                    AssetId::reissuance_token_from_entropy(asset_entropy, is_confidential)
                        .into_inner();

                issuances.push((
                    asset_id,
                    AssetRow {
                        issuance_txid: txid,
                        issuance_vin: txi_index as u16,
                        prev_txid: full_hash(&txi.previous_output.txid[..]),
                        prev_vout: txi.previous_output.vout as u16,
                        issuance: serialize(&txi.asset_issuance),
                        reissuance_token: full_hash(&reissuance_token[..]),
                    },
                ));
            }
        }
    }

    (history, issuances)
}

// returns the asset id if its an explicit user-issued asset, or none for confidential and native assets
fn get_user_asset_id(asset: &Asset) -> Option<sha256d::Hash> {
    match asset {
        Asset::Explicit(asset_hash)
            if asset_hash != &*NATIVE_ASSET_ID && asset_hash != &*NATIVE_ASSET_ID_TESTNET =>
        {
            Some(*asset_hash)
        }
        _ => None,
    }
}

fn asset_history_row(
    asset_id: &sha256d::Hash,
    confirmed_height: u32,
    txinfo: TxHistoryInfo,
) -> TxHistoryRow {
    let key = TxHistoryKey {
        code: b'I',
        hash: full_hash(&asset_id[..]),
        confirmed_height,
        txinfo,
    };
    TxHistoryRow { key }
}

pub fn lookup_asset(
    chain: &ChainQuery,
    registry: Option<&AssetRegistry>,
    asset_hash: &[u8],
) -> Result<Option<AssetEntry>> {
    let history_db = chain.store().history_db();

    if let Some(row) = history_db.get(&[b"i", &asset_hash[..]].concat()) {
        let row = bincode::deserialize::<AssetRow>(&row).expect("failed to parse AssetRow");
        let asset_id = sha256d::Hash::from_slice(asset_hash).chain_err(|| "invalid asset hash")?;
        let meta = registry.map_or_else(|| Ok(None), |r| r.load(asset_id))?;
        let chain_stats = asset_stats(chain, asset_hash, &row.reissuance_token);
        Ok(Some(AssetEntry::new(asset_hash, row, chain_stats, meta)))
    } else {
        Ok(None)
    }
}

pub fn get_issuance_assetid(txin: &TxIn) -> Result<AssetId> {
    let entropy = get_issuance_entropy(txin)?;
    Ok(AssetId::from_entropy(entropy))
}

fn get_issuance_entropy(txin: &TxIn) -> Result<sha256::Midstate> {
    if !txin.has_issuance {
        bail!("input has no issuance");
    }

    let is_reissuance = txin.asset_issuance.asset_blinding_nonce != [0u8; 32];

    Ok(if !is_reissuance {
        let contract_hash = sha256::Hash::from_slice(&txin.asset_issuance.asset_entropy)
            .chain_err(|| "invalid entropy (contract hash)")?;
        AssetId::generate_asset_entropy(txin.previous_output.clone(), contract_hash)
    } else {
        sha256::Midstate::from_slice(&txin.asset_issuance.asset_entropy)
            .chain_err(|| "invalid entropy (reissuance)")?
    })
}

// Asset stats

#[derive(Serialize, Deserialize, Debug)]
pub struct AssetStats {
    pub tx_count: usize,
    pub issuance_count: usize,
    pub issued_amount: u64,
    pub burned_amount: u64,
    pub has_blinded_issuances: bool,
    pub reissuance_tokens: Option<u64>, // none if confidential
    pub burned_reissuance_tokens: u64,
}

impl AssetStats {
    fn default() -> Self {
        Self {
            tx_count: 0,
            issuance_count: 0,
            issued_amount: 0,
            burned_amount: 0,
            has_blinded_issuances: false,
            reissuance_tokens: None,
            burned_reissuance_tokens: 0,
        }
    }
}

fn asset_cache_key(asset_hash: &[u8]) -> Bytes {
    [b"z", asset_hash].concat()
}
fn asset_cache_row(asset_hash: &[u8], stats: &AssetStats, blockhash: &sha256d::Hash) -> DBRow {
    DBRow {
        key: asset_cache_key(asset_hash),
        value: bincode::serialize(&(stats, blockhash)).unwrap(),
    }
}

fn asset_stats(chain: &ChainQuery, asset_hash: &[u8], reissuance_token: &[u8]) -> AssetStats {
    let mut stats = _asset_stats(chain, asset_hash);
    stats.burned_reissuance_tokens = _asset_stats(chain, reissuance_token).burned_amount;
    stats
}

fn _asset_stats(chain: &ChainQuery, asset_hash: &[u8]) -> AssetStats {
    // get the last known stats and the blockhash they are updated for.
    // invalidates the cache if the block was orphaned.
    let cache: Option<(AssetStats, usize)> = chain
        .store()
        .cache_db()
        .get(&asset_cache_key(asset_hash))
        .map(|c| bincode::deserialize(&c).unwrap())
        .and_then(|(stats, blockhash)| {
            chain
                .height_by_hash(&blockhash)
                .map(|height| (stats, height))
        });

    // update stats with new transactions since
    let (newstats, lastblock) = cache.map_or_else(
        || asset_stats_delta(chain, asset_hash, AssetStats::default(), 0),
        |(oldstats, blockheight)| asset_stats_delta(chain, asset_hash, oldstats, blockheight + 1),
    );

    // save updated stats to cache
    if let Some(lastblock) = lastblock {
        chain.store().cache_db().write(
            vec![asset_cache_row(asset_hash, &newstats, &lastblock)],
            DBFlush::Enable,
        );
    }

    newstats
}

fn asset_stats_delta(
    chain: &ChainQuery,
    asset_hash: &[u8],
    init_stats: AssetStats,
    start_height: usize,
) -> (AssetStats, Option<sha256d::Hash>) {
    let history_iter = chain
        .history_iter_scan(b'I', asset_hash, start_height)
        .map(TxHistoryRow::from_row)
        .filter_map(|history| {
            chain
                .tx_confirming_block(&history.get_txid())
                .map(|blockid| (history, blockid))
        });

    let mut stats = init_stats;
    let mut seen_txids = HashSet::new();
    let mut lastblock = None;

    for (history, blockid) in history_iter {
        if lastblock != Some(blockid.hash) {
            seen_txids.clear();
        }

        if seen_txids.insert(history.get_txid()) {
            stats.tx_count += 1;
        }

        match history.key.txinfo {
            TxHistoryInfo::Funding(_) | TxHistoryInfo::Spending(_) => {
                // no fund/spend stats for now
            }

            TxHistoryInfo::Issuing(issuance) => {
                stats.issuance_count += 1;

                match issuance.issued_amount {
                    Some(amount) => stats.issued_amount += amount,
                    None => stats.has_blinded_issuances = true,
                }

                if !issuance.is_reissuance {
                    stats.reissuance_tokens = issuance.token_amount;
                }
            }

            TxHistoryInfo::Burning(info) => {
                if let Value::Explicit(value) = info.value {
                    stats.burned_amount += value;
                }
            }
        }

        lastblock = Some(blockid.hash);
    }

    (stats, lastblock)
}
