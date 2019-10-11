use std::collections::{HashMap, HashSet};

use bitcoin::hashes::{hex::FromHex, sha256, sha256d, Hash};
use elements::confidential::{Asset, Value};
use elements::encode::{deserialize, serialize};
use elements::{AssetIssuance, OutPoint, Transaction, TxIn};

use crate::errors::*;
use crate::new_index::schema::{FundingInfo, TxHistoryInfo, TxHistoryKey, TxHistoryRow};
use crate::new_index::{db::DBFlush, parse_hash, ChainQuery, DBRow, Mempool, Query};
use crate::util::{full_hash, is_spendable, Bytes, FullHash, TransactionStatus, TxInput};

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
    static ref NATIVE_ASSET: NativeAsset = NativeAsset {
        asset_id: *NATIVE_ASSET_ID,
        meta: AssetMeta {
            contract: json!(null),
            entity: json!(null),
            precision: 8,
            name: "Liquid Bitcoin".into(),
            ticker: Some("L-BTC".into()),
        }
    };
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum LiquidAsset {
    Issued(IssuedAsset),
    Native(NativeAsset),
}

#[derive(Serialize, Clone)]
pub struct NativeAsset {
    pub asset_id: sha256d::Hash, // not really a sha256d
    #[serde(flatten)]
    pub meta: AssetMeta,
}

#[derive(Serialize)]
pub struct IssuedAsset {
    pub asset_id: sha256d::Hash, // not really a sha256d
    pub issuance_txin: TxInput,
    pub issuance_prevout: OutPoint,
    pub reissuance_token: sha256d::Hash, // not really a sha256d

    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_hash: Option<sha256d::Hash>, // not really a sha256d

    // the confirmation status of the initial issuance transaction
    pub status: TransactionStatus,

    pub chain_stats: AssetStats,
    pub mempool_stats: AssetStats,

    // optional metadata from registry
    #[serde(flatten)]
    pub meta: Option<AssetMeta>,
}

// DB representation (issued assets only)
#[derive(Serialize, Deserialize, Debug)]
pub struct AssetRow {
    pub issuance_txid: FullHash,
    pub issuance_vin: u16,
    pub prev_txid: FullHash,
    pub prev_vout: u16,
    pub issuance: Bytes, // bincode does not like dealing with AssetIssuance, deserialization fails with "invalid type: sequence, expected a struct"
    pub reissuance_token: FullHash,
}

impl IssuedAsset {
    pub fn new(
        asset_id: &sha256d::Hash,
        asset: &AssetRow,
        (chain_stats, mempool_stats): (AssetStats, AssetStats),
        meta: Option<AssetMeta>,
        status: TransactionStatus,
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
            asset_id: parse_hash(&full_hash(&asset_id[..])),
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
            status,
            chain_stats,
            mempool_stats,
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

// Index confirmed transaction issuances and save as db rows
pub fn index_confirmed_tx_assets(tx: &Transaction, confirmed_height: u32, rows: &mut Vec<DBRow>) {
    let (history, issuances) = index_tx_assets(tx);

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

// Index mempool transaction issuances and save to in-memory store
pub fn index_mempool_tx_assets(
    tx: &Transaction,
    asset_history: &mut HashMap<sha256d::Hash, Vec<TxHistoryInfo>>,
    asset_issuance: &mut HashMap<sha256d::Hash, AssetRow>,
) {
    let (history, issuances) = index_tx_assets(tx);
    for (asset_id, info) in history {
        asset_history
            .entry(asset_id)
            .or_insert_with(|| Vec::new())
            .push(info);
    }
    for (asset_id, issuance) in issuances {
        asset_issuance.insert(asset_id, issuance);
    }
}

// Remove mempool transaction issuances from in-memory store
pub fn remove_mempool_tx_assets(
    to_remove: &HashSet<&sha256d::Hash>,
    asset_history: &mut HashMap<sha256d::Hash, Vec<TxHistoryInfo>>,
    asset_issuance: &mut HashMap<sha256d::Hash, AssetRow>,
) {
    // TODO optimize
    asset_history.retain(|_assethash, entries| {
        entries.retain(|entry| !to_remove.contains(&entry.get_txid()));
        !entries.is_empty()
    });

    asset_issuance
        .retain(|_assethash, issuance| !to_remove.contains(&parse_hash(&issuance.issuance_txid)));
}

// Internal utility function, index a transaction and return its history entries and issuances
fn index_tx_assets(
    tx: &Transaction,
) -> (
    Vec<(sha256d::Hash, TxHistoryInfo)>,
    Vec<(sha256d::Hash, AssetRow)>,
) {
    let mut history = vec![];
    let mut issuances = vec![];

    let txid = full_hash(&tx.txid()[..]);
    for (txo_index, txo) in tx.output.iter().enumerate() {
        if !is_spendable(txo) {
            if let Some(asset_id) = get_issued_asset_id(&txo.asset) {
                history.push((
                    asset_id,
                    TxHistoryInfo::Burning(FundingInfo {
                        txid,
                        vout: txo_index as u16,
                        value: txo.value,
                    }),
                ));
            }
        }
    }

    for (txi_index, txi) in tx.input.iter().enumerate() {
        if txi.has_issuance() {
            let is_reissuance = txi.asset_issuance.asset_blinding_nonce != [0u8; 32];

            let asset_entropy = get_issuance_entropy(txi).expect("invalid issuance");
            let asset_id = AssetId::from_entropy(asset_entropy.clone());
            // ugh, should eventually switch to using AssetIds everywhere
            let asset_id = sha256d::Hash::from_inner(asset_id.into_inner().into_inner());

            let issued_amount = match txi.asset_issuance.amount {
                Value::Explicit(amount) => Some(amount),
                Value::Null => Some(0),
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
fn get_issued_asset_id(asset: &Asset) -> Option<sha256d::Hash> {
    match asset {
        Asset::Explicit(asset_id)
            if asset_id != &*NATIVE_ASSET_ID && asset_id != &*NATIVE_ASSET_ID_TESTNET =>
        {
            Some(*asset_id)
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
    query: &Query,
    registry: Option<&AssetRegistry>,
    asset_id: &sha256d::Hash,
) -> Result<Option<LiquidAsset>> {
    if asset_id == &*NATIVE_ASSET_ID {
        return Ok(Some(LiquidAsset::Native(NATIVE_ASSET.clone())));
    }

    let history_db = query.chain().store().history_db();
    let mempool_issuances = &query.mempool().asset_issuance;

    let chain_row = history_db
        .get(&[b"i", &asset_id[..]].concat())
        .map(|row| bincode::deserialize::<AssetRow>(&row).expect("failed parsing AssetRow"));

    let row = chain_row
        .as_ref()
        .or_else(|| mempool_issuances.get(asset_id));

    Ok(if let Some(row) = row {
        let reissuance_token = sha256d::Hash::from_slice(&row.reissuance_token)
            .expect("failed parsing reissuance_token");

        let meta = registry.map_or_else(|| Ok(None), |r| r.load(asset_id))?;
        let stats = asset_stats(query, asset_id, &reissuance_token);
        let status = query.get_tx_status(&parse_hash(&row.issuance_txid));

        let asset = IssuedAsset::new(asset_id, row, stats, meta, status);

        Some(LiquidAsset::Issued(asset))
    } else {
        None
    })
}

pub fn get_issuance_entropy(txin: &TxIn) -> Result<sha256::Midstate> {
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

fn asset_cache_key(asset_id: &sha256d::Hash) -> Bytes {
    [b"z", &asset_id[..]].concat()
}
fn asset_cache_row(
    asset_id: &sha256d::Hash,
    stats: &AssetStats,
    blockhash: &sha256d::Hash,
) -> DBRow {
    DBRow {
        key: asset_cache_key(asset_id),
        value: bincode::serialize(&(stats, blockhash)).unwrap(),
    }
}

fn asset_stats(
    query: &Query,
    asset_id: &sha256d::Hash,
    reissuance_token: &sha256d::Hash,
) -> (AssetStats, AssetStats) {
    let chain = query.chain();
    let mut chain_stats = chain_asset_stats(chain, asset_id);
    chain_stats.burned_reissuance_tokens = chain_asset_stats(chain, reissuance_token).burned_amount;

    let mempool = query.mempool();
    let mut mempool_stats = mempool_asset_stats(&mempool, &asset_id);
    mempool_stats.burned_reissuance_tokens =
        mempool_asset_stats(&mempool, &reissuance_token).burned_amount;

    (chain_stats, mempool_stats)
}

fn chain_asset_stats(chain: &ChainQuery, asset_id: &sha256d::Hash) -> AssetStats {
    // get the last known stats and the blockhash they are updated for.
    // invalidates the cache if the block was orphaned.
    let cache: Option<(AssetStats, usize)> = chain
        .store()
        .cache_db()
        .get(&asset_cache_key(asset_id))
        .map(|c| bincode::deserialize(&c).unwrap())
        .and_then(|(stats, blockhash)| {
            chain
                .height_by_hash(&blockhash)
                .map(|height| (stats, height))
        });

    // update stats with new transactions since
    let (newstats, lastblock) = cache.map_or_else(
        || asset_stats_delta(chain, asset_id, AssetStats::default(), 0),
        |(oldstats, blockheight)| asset_stats_delta(chain, asset_id, oldstats, blockheight + 1),
    );

    // save updated stats to cache
    if let Some(lastblock) = lastblock {
        chain.store().cache_db().write(
            vec![asset_cache_row(asset_id, &newstats, &lastblock)],
            DBFlush::Enable,
        );
    }

    newstats
}

fn asset_stats_delta(
    chain: &ChainQuery,
    asset_id: &sha256d::Hash,
    init_stats: AssetStats,
    start_height: usize,
) -> (AssetStats, Option<sha256d::Hash>) {
    let history_iter = chain
        .history_iter_scan(b'I', &asset_id[..], start_height)
        .map(TxHistoryRow::from_row)
        .filter_map(|history| {
            chain
                .tx_confirming_block(&history.get_txid())
                .map(|blockid| (history, blockid))
        });

    let mut stats = init_stats;
    let mut seen_txids = HashSet::new();
    let mut lastblock = None;

    for (row, blockid) in history_iter {
        if lastblock != Some(blockid.hash) {
            seen_txids.clear();
        }
        apply_asset_stats(&row.key.txinfo, &mut stats, &mut seen_txids);
        lastblock = Some(blockid.hash);
    }

    (stats, lastblock)
}

pub fn mempool_asset_stats(mempool: &Mempool, asset_id: &sha256d::Hash) -> AssetStats {
    let mut stats = AssetStats::default();

    if let Some(history) = mempool.asset_history.get(asset_id) {
        let mut seen_txids = HashSet::new();
        for info in history {
            apply_asset_stats(info, &mut stats, &mut seen_txids)
        }
    }

    stats
}

fn apply_asset_stats(
    info: &TxHistoryInfo,
    stats: &mut AssetStats,
    seen_txids: &mut HashSet<sha256d::Hash>,
) {
    if seen_txids.insert(info.get_txid()) {
        stats.tx_count += 1;
    }

    match info {
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

        TxHistoryInfo::Funding(_) | TxHistoryInfo::Spending(_) => {
            // we don't keep funding/spending entries for assets
            unreachable!();
        }
    }
}
