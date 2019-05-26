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
    pub asset_id: sha256d::Hash,
    pub issuance_txin: TxInput,
    pub issuance_prevout: OutPoint,
    pub contract_hash: sha256d::Hash,

    pub chain_stats: AssetStats,

    // optional metadata from registry
    #[serde(flatten)]
    pub meta: Option<AssetMeta>,
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

impl AssetEntry {
    pub fn new(
        asset_hash: &[u8],
        asset: AssetRowValue,
        chain_stats: AssetStats,
        meta: Option<AssetMeta>,
    ) -> Self {
        let issuance: AssetIssuance =
            deserialize(&asset.issuance).expect("failed parsing AssetIssuance");

        // XXX this isn't really a double-hash, sha256d is only being used to get backward
        // serialization that matches the one used by elements-cpp
        let contract_hash = sha256d::Hash::from_inner(issuance.asset_entropy);

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
            chain_stats,
            meta,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct IssuanceInfo {
    pub txid: FullHash,
    pub vin: u16,
    pub is_reissuance: bool,
    // None for blinded issuances
    pub issued_amount: Option<u64>,
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
            let is_reissuance = txi.asset_issuance.asset_blinding_nonce != [0u8; 32];

            let asset_id = get_issuance_assetid(txi).expect("invalid issuance");
            let asset_hash = asset_id.into_inner().into_inner();
            let asset = Asset::Explicit(sha256d::Hash::from_inner(asset_hash.clone()));

            let issued_amount = match txi.asset_issuance.amount {
                Value::Explicit(amount) => Some(amount),
                _ => None,
            };

            // the initial issuance is kept twice: once in the history index under I<asset><height><txid:vin>,
            // and once separately under i<asset> for asset lookup with some more associated metadata.
            // reissuances are only kept under the history index.

            let history = asset_history_row(
                &asset,
                confirmed_height,
                TxHistoryInfo::Issuance(IssuanceInfo {
                    txid,
                    vin: txi_index as u16,
                    is_reissuance,
                    issued_amount,
                }),
            );
            rows.push(history.to_row());

            if !is_reissuance {
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

pub fn lookup_asset(
    chain: &ChainQuery,
    registry: Option<&AssetRegistry>,
    asset_hash: &[u8],
) -> Result<Option<AssetEntry>> {
    let history_db = chain.store().history_db();

    if let Some(row) = history_db.get(&[b"i", &asset_hash[..]].concat()) {
        let row = bincode::deserialize(&row).expect("failed to parse AssetRowValue");
        let asset_id = sha256d::Hash::from_slice(asset_hash).chain_err(|| "invalid asset hash")?;
        let meta = registry.map_or_else(|| Ok(None), |r| r.load(asset_id))?;
        let chain_stats = asset_stats(chain, asset_hash);
        Ok(Some(AssetEntry::new(asset_hash, row, chain_stats, meta)))
    } else {
        Ok(None)
    }
}

pub fn get_issuance_assetid(txin: &TxIn) -> Result<AssetId> {
    if !txin.has_issuance {
        bail!("input has no issuance");
    }

    let is_reissuance = txin.asset_issuance.asset_blinding_nonce != [0u8; 32];

    let entropy = if !is_reissuance {
        let contract_hash = sha256::Hash::from_slice(&txin.asset_issuance.asset_entropy)
            .chain_err(|| "invalid entropy (contract hash)")?;
        AssetId::generate_asset_entropy(txin.previous_output.clone(), contract_hash)
    } else {
        sha256::Midstate::from_slice(&txin.asset_issuance.asset_entropy)
            .chain_err(|| "invalid entropy (reissuance)")?
    };

    Ok(AssetId::from_entropy(entropy))
}

// Asset stats

#[derive(Serialize, Deserialize, Debug)]
pub struct AssetStats {
    pub tx_count: usize,
    pub issuance_count: usize,
    pub issued_amount: u64,
    pub has_blinded_issuances: bool,
}

impl AssetStats {
    fn default() -> Self {
        Self {
            tx_count: 0,
            issuance_count: 0,
            issued_amount: 0,
            has_blinded_issuances: false,
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

pub fn asset_stats(chain: &ChainQuery, asset_hash: &[u8]) -> AssetStats {
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
            TxHistoryInfo::Funding(_) => {}

            TxHistoryInfo::Spending(_) => {}

            TxHistoryInfo::Issuance(issuance) => {
                stats.issuance_count += 1;

                match issuance.issued_amount {
                    Some(amount) => stats.issued_amount += amount,
                    None => stats.has_blinded_issuances = true,
                }
            }
        }

        lastblock = Some(blockid.hash);
    }

    (stats, lastblock)
}
