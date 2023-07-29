use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use bitcoin::hashes::{hex::FromHex, sha256, Hash};
use elements::confidential::{Asset, Value};
use elements::encode::{deserialize, serialize};
use elements::secp256k1_zkp::ZERO_TWEAK;
use elements::{issuance::ContractHash, AssetId, AssetIssuance, OutPoint, Transaction, TxIn};

use crate::chain::{BNetwork, BlockHash, Network, Txid};
use crate::elements::peg::{get_pegin_data, get_pegout_data, PeginInfo, PegoutInfo};
use crate::elements::registry::{AssetMeta, AssetRegistry};
use crate::errors::*;
use crate::new_index::schema::{TxHistoryInfo, TxHistoryKey, TxHistoryRow};
use crate::new_index::{db::DBFlush, ChainQuery, DBRow, Mempool, Query};
use crate::util::{full_hash, Bytes, FullHash, TransactionStatus, TxInput};

lazy_static! {
    pub static ref NATIVE_ASSET_ID: AssetId =
        AssetId::from_hex("6f0279e9ed041c3d710a9f57d0c02928416460c4b722ae3457a11eec381c526d")
            .unwrap();
    pub static ref NATIVE_ASSET_ID_TESTNET: AssetId =
        AssetId::from_hex("144c654344aa716d6f3abcc1ca90e5641e4e2a7f633bc09fe3baf64585819a49")
            .unwrap();
    pub static ref NATIVE_ASSET_ID_REGTEST: AssetId =
        AssetId::from_hex("5ac9f65c0efcc4775e0baec4ec03abdde22473cd3cf33c0419ca290e0751b225")
            .unwrap();
}

fn parse_asset_id(sl: &[u8]) -> AssetId {
    AssetId::from_slice(sl).expect("failed to parse AssetId")
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum LiquidAsset {
    Issued(IssuedAsset),
    Native(PeggedAsset),
}

#[derive(Serialize)]
pub struct PeggedAsset {
    pub asset_id: AssetId,
    pub chain_stats: PeggedAssetStats,
    pub mempool_stats: PeggedAssetStats,
}

#[derive(Serialize)]
pub struct IssuedAsset {
    pub asset_id: AssetId,
    pub issuance_txin: TxInput,
    #[serde(serialize_with = "crate::util::serialize_outpoint")]
    pub issuance_prevout: OutPoint,
    pub reissuance_token: AssetId,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_hash: Option<ContractHash>,

    // the confirmation status of the initial issuance transaction
    pub status: TransactionStatus,

    pub chain_stats: IssuedAssetStats,
    pub mempool_stats: IssuedAssetStats,

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
        asset_id: &AssetId,
        asset: &AssetRow,
        (chain_stats, mempool_stats): (IssuedAssetStats, IssuedAssetStats),
        meta: Option<AssetMeta>,
        status: TransactionStatus,
    ) -> Self {
        let issuance: AssetIssuance =
            deserialize(&asset.issuance).expect("failed parsing AssetIssuance");

        let reissuance_token = parse_asset_id(&asset.reissuance_token);

        let contract_hash = if issuance.asset_entropy != [0u8; 32] {
            Some(ContractHash::from_inner(issuance.asset_entropy))
        } else {
            None
        };

        Self {
            asset_id: *asset_id,
            issuance_txin: TxInput {
                txid: deserialize(&asset.issuance_txid).unwrap(),
                vin: asset.issuance_vin,
            },
            issuance_prevout: OutPoint {
                txid: deserialize(&asset.prev_txid).unwrap(),
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

impl LiquidAsset {
    pub fn supply(&self) -> Option<u64> {
        match self {
            LiquidAsset::Native(asset) => Some(
                asset.chain_stats.peg_in_amount
                    - asset.chain_stats.peg_out_amount
                    - asset.chain_stats.burned_amount
                    + asset.mempool_stats.peg_in_amount
                    - asset.mempool_stats.peg_out_amount
                    - asset.mempool_stats.burned_amount,
            ),
            LiquidAsset::Issued(asset) => {
                if asset.chain_stats.has_blinded_issuances
                    || asset.mempool_stats.has_blinded_issuances
                {
                    None
                } else {
                    Some(
                        asset.chain_stats.issued_amount - asset.chain_stats.burned_amount
                            + asset.mempool_stats.issued_amount
                            - asset.mempool_stats.burned_amount,
                    )
                }
            }
        }
    }
    pub fn precision(&self) -> u8 {
        match self {
            LiquidAsset::Native(_) => 8,
            LiquidAsset::Issued(asset) => asset.meta.as_ref().map_or(0, |m| m.precision),
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

#[derive(Serialize, Deserialize, Debug)]
pub struct BurningInfo {
    pub txid: FullHash,
    pub vout: u16,
    pub value: u64,
}

// Index confirmed transaction issuances and save as db rows
pub fn index_confirmed_tx_assets(
    tx: &Transaction,
    confirmed_height: u32,
    network: Network,
    parent_network: BNetwork,
    rows: &mut Vec<DBRow>,
) {
    let (history, issuances) = index_tx_assets(tx, network, parent_network);

    rows.extend(
        history.into_iter().map(|(asset_id, info)| {
            asset_history_row(&asset_id, confirmed_height, info).into_row()
        }),
    );

    // the initial issuance is kept twice: once in the history index under I<asset><height><txid:vin>,
    // and once separately under i<asset> for asset lookup with some more associated metadata.
    // reissuances are only kept under the history index.
    rows.extend(issuances.into_iter().map(|(asset_id, asset_row)| DBRow {
        key: [b"i", &asset_id.into_inner()[..]].concat(),
        value: bincode::serialize(&asset_row).unwrap(),
    }));
}

// Index mempool transaction issuances and save to in-memory store
pub fn index_mempool_tx_assets(
    tx: &Transaction,
    network: Network,
    parent_network: BNetwork,
    asset_history: &mut HashMap<AssetId, Vec<TxHistoryInfo>>,
    asset_issuance: &mut HashMap<AssetId, AssetRow>,
) {
    let (history, issuances) = index_tx_assets(tx, network, parent_network);
    for (asset_id, info) in history {
        asset_history
            .entry(asset_id)
            .or_insert_with(Vec::new)
            .push(info);
    }
    for (asset_id, issuance) in issuances {
        asset_issuance.insert(asset_id, issuance);
    }
}

// Remove mempool transaction issuances from in-memory store
pub fn remove_mempool_tx_assets(
    to_remove: &HashSet<&Txid>,
    asset_history: &mut HashMap<AssetId, Vec<TxHistoryInfo>>,
    asset_issuance: &mut HashMap<AssetId, AssetRow>,
) {
    // TODO optimize
    asset_history.retain(|_assethash, entries| {
        entries.retain(|entry| !to_remove.contains(&entry.get_txid()));
        !entries.is_empty()
    });

    asset_issuance.retain(|_assethash, issuance| {
        let txid: Txid = deserialize(&issuance.issuance_txid).unwrap();
        !to_remove.contains(&txid)
    });
}

// Internal utility function, index a transaction and return its history entries and issuances
fn index_tx_assets(
    tx: &Transaction,
    network: Network,
    parent_network: BNetwork,
) -> (Vec<(AssetId, TxHistoryInfo)>, Vec<(AssetId, AssetRow)>) {
    let mut history = vec![];
    let mut issuances = vec![];

    let txid = full_hash(&tx.txid()[..]);

    for (txo_index, txo) in tx.output.iter().enumerate() {
        if let Some(pegout) = get_pegout_data(txo, network, parent_network) {
            history.push((
                pegout.asset.explicit().unwrap(),
                TxHistoryInfo::Pegout(PegoutInfo {
                    txid,
                    vout: txo_index as u16,
                    value: pegout.value,
                }),
            ));
        } else if txo.script_pubkey.is_provably_unspendable() && !txo.is_fee() {
            if let (Asset::Explicit(asset_id), Value::Explicit(value)) = (txo.asset, txo.value) {
                if value > 0 {
                    history.push((
                        asset_id,
                        TxHistoryInfo::Burning(BurningInfo {
                            txid,
                            vout: txo_index as u16,
                            value: value,
                        }),
                    ));
                }
            }
        }
    }

    for (txi_index, txi) in tx.input.iter().enumerate() {
        if let Some(pegin) = get_pegin_data(txi, network) {
            history.push((
                pegin.asset.explicit().unwrap(),
                TxHistoryInfo::Pegin(PeginInfo {
                    txid,
                    vin: txi_index as u16,
                    value: pegin.value,
                }),
            ));
        } else if txi.has_issuance() {
            let is_reissuance = txi.asset_issuance.asset_blinding_nonce != ZERO_TWEAK;

            let asset_entropy = get_issuance_entropy(txi).expect("invalid issuance");
            let asset_id = AssetId::from_entropy(asset_entropy);

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
                    AssetId::reissuance_token_from_entropy(asset_entropy, is_confidential);

                issuances.push((
                    asset_id,
                    AssetRow {
                        issuance_txid: txid,
                        issuance_vin: txi_index as u16,
                        prev_txid: full_hash(&txi.previous_output.txid[..]),
                        prev_vout: txi.previous_output.vout as u16,
                        issuance: serialize(&txi.asset_issuance),
                        reissuance_token: full_hash(&reissuance_token.into_inner()[..]),
                    },
                ));
            }
        }
    }

    (history, issuances)
}

fn asset_history_row(
    asset_id: &AssetId,
    confirmed_height: u32,
    txinfo: TxHistoryInfo,
) -> TxHistoryRow {
    let key = TxHistoryKey {
        code: b'I',
        hash: full_hash(&asset_id.into_inner()[..]),
        confirmed_height,
        txinfo,
    };
    TxHistoryRow { key }
}

pub fn lookup_asset(
    query: &Query,
    registry: Option<&Arc<RwLock<AssetRegistry>>>,
    asset_id: &AssetId,
    meta: Option<&AssetMeta>, // may optionally be provided if already known
) -> Result<Option<LiquidAsset>> {
    if query.network().pegged_asset() == Some(asset_id) {
        let (chain_stats, mempool_stats) = pegged_asset_stats(query, asset_id);

        return Ok(Some(LiquidAsset::Native(PeggedAsset {
            asset_id: *asset_id,
            chain_stats: chain_stats,
            mempool_stats: mempool_stats,
        })));
    }

    let history_db = query.chain().store().history_db();
    let mempool = query.mempool();
    let mempool_issuances = &mempool.asset_issuance;

    let chain_row = history_db
        .get(&[b"i", &asset_id.into_inner()[..]].concat())
        .map(|row| bincode::deserialize::<AssetRow>(&row).expect("failed parsing AssetRow"));

    let row = chain_row
        .as_ref()
        .or_else(|| mempool_issuances.get(asset_id));

    Ok(if let Some(row) = row {
        let reissuance_token = parse_asset_id(&row.reissuance_token);

        let meta = meta
            .cloned()
            .or_else(|| registry.and_then(|r| r.read().unwrap().get(asset_id).cloned()));
        let stats = issued_asset_stats(query.chain(), &mempool, asset_id, &reissuance_token);
        let status = query.get_tx_status(&deserialize(&row.issuance_txid).unwrap());

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

    let is_reissuance = txin.asset_issuance.asset_blinding_nonce != ZERO_TWEAK;

    Ok(if !is_reissuance {
        let contract_hash = ContractHash::from_slice(&txin.asset_issuance.asset_entropy)
            .chain_err(|| "invalid entropy (contract hash)")?;
        AssetId::generate_asset_entropy(txin.previous_output, contract_hash)
    } else {
        sha256::Midstate::from_slice(&txin.asset_issuance.asset_entropy)
            .chain_err(|| "invalid entropy (reissuance)")?
    })
}

//
// Asset stats
//

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct IssuedAssetStats {
    pub tx_count: usize,
    pub issuance_count: usize,
    pub issued_amount: u64,
    pub burned_amount: u64,
    pub has_blinded_issuances: bool,
    pub reissuance_tokens: Option<u64>, // none if confidential
    pub burned_reissuance_tokens: u64,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct PeggedAssetStats {
    pub tx_count: usize,
    pub peg_in_count: usize,
    pub peg_in_amount: u64,
    pub peg_out_count: usize,
    pub peg_out_amount: u64,
    pub burn_count: usize,
    pub burned_amount: u64,
}

type AssetStatApplyFn<T> = fn(&TxHistoryInfo, &mut T, &mut HashSet<Txid>);

fn asset_cache_key(asset_id: &AssetId) -> Bytes {
    [b"z", &asset_id.into_inner()[..]].concat()
}

fn asset_cache_row<T>(asset_id: &AssetId, stats: &T, blockhash: &BlockHash) -> DBRow
where
    T: serde::Serialize,
{
    DBRow {
        key: asset_cache_key(asset_id),
        value: bincode::serialize(&(stats, blockhash)).unwrap(),
    }
}

// Get stats for the network's pegged asset
fn pegged_asset_stats(query: &Query, asset_id: &AssetId) -> (PeggedAssetStats, PeggedAssetStats) {
    (
        chain_asset_stats(query.chain(), asset_id, apply_pegged_asset_stats),
        mempool_asset_stats(&query.mempool(), asset_id, apply_pegged_asset_stats),
    )
}

// Get stats for issued assets
fn issued_asset_stats(
    chain: &ChainQuery,
    mempool: &Mempool,
    asset_id: &AssetId,
    reissuance_token: &AssetId,
) -> (IssuedAssetStats, IssuedAssetStats) {
    let afn = apply_issued_asset_stats;

    let mut chain_stats = chain_asset_stats(chain, asset_id, afn);
    chain_stats.burned_reissuance_tokens =
        chain_asset_stats(chain, reissuance_token, afn).burned_amount;

    let mut mempool_stats = mempool_asset_stats(&mempool, &asset_id, afn);
    mempool_stats.burned_reissuance_tokens =
        mempool_asset_stats(mempool, &reissuance_token, afn).burned_amount;

    (chain_stats, mempool_stats)
}

// Get on-chain confirmed asset stats (issued or the pegged asset)
fn chain_asset_stats<T>(chain: &ChainQuery, asset_id: &AssetId, apply_fn: AssetStatApplyFn<T>) -> T
where
    T: Default + serde::Serialize + serde::de::DeserializeOwned,
{
    // get the last known stats and the blockhash they are updated for.
    // invalidates the cache if the block was orphaned.
    let cache: Option<(T, usize)> = chain
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
        || chain_asset_stats_delta(chain, asset_id, T::default(), 0, apply_fn),
        |(oldstats, blockheight)| {
            chain_asset_stats_delta(chain, asset_id, oldstats, blockheight + 1, apply_fn)
        },
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

// Update the asset stats with the delta of confirmed txs since start_height
fn chain_asset_stats_delta<T>(
    chain: &ChainQuery,
    asset_id: &AssetId,
    init_stats: T,
    start_height: usize,
    apply_fn: AssetStatApplyFn<T>,
) -> (T, Option<BlockHash>) {
    let history_iter = chain
        .history_iter_scan(b'I', &asset_id.into_inner()[..], start_height)
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
        apply_fn(&row.key.txinfo, &mut stats, &mut seen_txids);
        lastblock = Some(blockid.hash);
    }

    (stats, lastblock)
}

// Get mempool asset stats (issued or the pegged asset)
pub fn mempool_asset_stats<T>(
    mempool: &Mempool,
    asset_id: &AssetId,
    apply_fn: AssetStatApplyFn<T>,
) -> T
where
    T: Default,
{
    let mut stats = T::default();

    if let Some(history) = mempool.asset_history.get(asset_id) {
        let mut seen_txids = HashSet::new();
        for info in history {
            apply_fn(info, &mut stats, &mut seen_txids)
        }
    }

    stats
}

fn apply_issued_asset_stats(
    info: &TxHistoryInfo,
    stats: &mut IssuedAssetStats,
    seen_txids: &mut HashSet<Txid>,
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
            stats.burned_amount += info.value;
        }

        TxHistoryInfo::Funding(_) | TxHistoryInfo::Spending(_) => {
            // we don't keep funding/spending entries for assets
            unreachable!();
        }

        TxHistoryInfo::Pegin(_) | TxHistoryInfo::Pegout(_) => {
            // issued assets cannot have pegins/pegouts
            unreachable!();
        }
    }
}

fn apply_pegged_asset_stats(
    info: &TxHistoryInfo,
    stats: &mut PeggedAssetStats,
    seen_txids: &mut HashSet<Txid>,
) {
    if seen_txids.insert(info.get_txid()) {
        stats.tx_count += 1;
    }

    match info {
        TxHistoryInfo::Pegin(info) => {
            stats.peg_in_count += 1;
            stats.peg_in_amount += info.value;
        }
        TxHistoryInfo::Pegout(info) => {
            stats.peg_out_count += 1;
            stats.peg_out_amount += info.value;
        }
        TxHistoryInfo::Burning(info) => {
            stats.burn_count += 1;
            stats.burned_amount += info.value;
        }
        TxHistoryInfo::Issuing(_) => {
            warn!("encountered issuance of native asset, ignoring (possibly freeinitialcoins?)");
        }
        TxHistoryInfo::Funding(_) | TxHistoryInfo::Spending(_) => {
            // these history entries variants are never kept for native assets
            unreachable!();
        }
    }
}
