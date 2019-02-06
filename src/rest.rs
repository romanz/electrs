use crate::chain::{Network, OutPoint, Transaction, TxIn, TxOut};
use crate::config::Config;
use crate::daemon::Daemon;
use crate::errors;
use crate::new_index::{compute_script_hash, Query, SpendingInput, Utxo};
use crate::util::Address;
use crate::util::{
    full_hash, get_script_asm, has_prevout, is_coinbase, script_to_address, BlockHeaderMeta,
    BlockId, FullHash, TransactionStatus,
};

#[cfg(feature = "liquid")]
use crate::util::{BlockProofValue, IssuanceValue, PegOutRequest};

use bitcoin::consensus::encode::{self, serialize};
use bitcoin::util::hash::{HexError, Sha256dHash};
use bitcoin::{BitcoinHash, Script};
use futures::sync::oneshot;
use hex::{self, FromHexError};
use hyper::rt::{self, Future};
use hyper::service::service_fn_ok;
use hyper::{Body, Method, Request, Response, Server, StatusCode};

#[cfg(feature = "liquid")]
use elements::confidential::{Asset, Value};

use serde::Serialize;
use serde_json;
use std::collections::HashMap;
use std::num::ParseIntError;
use std::str::FromStr;
use std::sync::Arc;
use std::thread;
use url::form_urlencoded;

const CHAIN_TXS_PER_PAGE: usize = 25;
const MAX_MEMPOOL_TXS: usize = 50;
const BLOCK_LIMIT: usize = 10;

const TTL_LONG: u32 = 157784630; // ttl for static resources (5 years)
const TTL_SHORT: u32 = 10; // ttl for volatie resources
const CONF_FINAL: usize = 10; // reorgs deeper than this are considered unlikely

#[derive(Serialize, Deserialize)]
struct BlockValue {
    id: String,
    height: u32,
    version: u32,
    timestamp: u32,
    tx_count: u32,
    size: u32,
    weight: u32,
    merkle_root: String,
    previousblockhash: Option<String>,
    #[cfg(not(feature = "liquid"))]
    nonce: u32,
    #[cfg(not(feature = "liquid"))]
    bits: u32,
    #[cfg(feature = "liquid")]
    proof: Option<BlockProofValue>,
}

impl From<BlockHeaderMeta> for BlockValue {
    fn from(blockhm: BlockHeaderMeta) -> Self {
        let header = blockhm.header_entry.header();
        BlockValue {
            id: header.bitcoin_hash().be_hex_string(),
            height: blockhm.header_entry.height() as u32,
            version: header.version,
            timestamp: header.time,
            tx_count: blockhm.meta.tx_count,
            size: blockhm.meta.size,
            weight: blockhm.meta.weight,
            merkle_root: header.merkle_root.be_hex_string(),
            previousblockhash: if &header.prev_blockhash != &Sha256dHash::default() {
                Some(header.prev_blockhash.be_hex_string())
            } else {
                None
            },

            #[cfg(not(feature = "liquid"))]
            bits: header.bits,
            #[cfg(not(feature = "liquid"))]
            nonce: header.nonce,

            #[cfg(feature = "liquid")]
            proof: Some(BlockProofValue::from(&header.proof)),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct TransactionValue {
    txid: Sha256dHash,
    version: u32,
    locktime: u32,
    vin: Vec<TxInValue>,
    vout: Vec<TxOutValue>,
    size: u32,
    weight: u32,
    fee: Option<u64>,
    status: Option<TransactionStatus>,
}

impl
    From<(
        Transaction,
        Option<BlockId>,
        &HashMap<OutPoint, TxOut>,
        &Config,
    )> for TransactionValue
{
    fn from(
        (tx, blockid, prevouts, config): (
            Transaction,
            Option<BlockId>,
            &HashMap<OutPoint, TxOut>,
            &Config,
        ),
    ) -> Self {
        let vins: Vec<TxInValue> = tx
            .input
            .iter()
            .map(|txin| {
                let prevout = prevouts.get(&txin.previous_output);
                TxInValue::from((txin.clone(), prevout, config)) // TODO avoid clone
            })
            .collect();
        let vouts: Vec<TxOutValue> = tx
            .output
            .iter()
            .map(|txout| TxOutValue::from((txout.clone(), config))) // TODO avoid clone
            .collect();
        let bytes = serialize(&tx);

        #[cfg(not(feature = "liquid"))]
        let fee = if config.prevout_enabled && !vins.iter().any(|vin| vin.prevout.is_none()) {
            let total_in: u64 = vins
                .iter()
                .map(|vin| vin.prevout.as_ref().unwrap().value)
                .sum();
            let total_out: u64 = vouts.iter().map(|vout| vout.value).sum();
            Some(total_in - total_out)
        } else {
            None
        };

        #[cfg(feature = "liquid")]
        let fee = vouts
            .iter()
            .find(|vout| vout.scriptpubkey_type == "fee")
            .map(|vout| vout.value.unwrap())
            .or_else(|| Some(0));

        TransactionValue {
            txid: tx.txid(),
            version: tx.version,
            locktime: tx.lock_time,
            vin: vins,
            vout: vouts,
            size: bytes.len() as u32,
            weight: tx.get_weight() as u32,
            fee,
            status: Some(TransactionStatus::from(blockid)),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct TxInValue {
    txid: Sha256dHash,
    vout: u32,
    prevout: Option<TxOutValue>,
    scriptsig: Script,
    scriptsig_asm: String,
    witness: Option<Vec<String>>,
    is_coinbase: bool,
    sequence: u32,

    #[cfg(feature = "liquid")]
    is_pegin: bool,
    #[cfg(feature = "liquid")]
    issuance: Option<IssuanceValue>,
}

impl From<(TxIn, Option<&TxOut>, &Config)> for TxInValue {
    fn from((txin, prevout, config): (TxIn, Option<&TxOut>, &Config)) -> Self {
        #[cfg(not(feature = "liquid"))]
        let witness = if txin.witness.len() > 0 {
            Some(txin.witness.iter().map(|w| hex::encode(w)).collect())
        } else {
            None
        };
        #[cfg(feature = "liquid")]
        let witness = None; // @TODO

        let is_coinbase = is_coinbase(&txin);

        TxInValue {
            txid: txin.previous_output.txid,
            vout: txin.previous_output.vout,
            prevout: prevout.map(|prevout| TxOutValue::from((prevout.clone(), config))), // TODO avoid clone()
            scriptsig_asm: get_script_asm(&txin.script_sig),
            witness,
            is_coinbase,
            sequence: txin.sequence,
            #[cfg(feature = "liquid")]
            is_pegin: txin.is_pegin,
            #[cfg(feature = "liquid")]
            issuance: if txin.has_issuance() {
                Some(IssuanceValue::from(&txin.asset_issuance))
            } else {
                None
            },

            scriptsig: txin.script_sig,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct TxOutValue {
    scriptpubkey: Script,
    scriptpubkey_asm: String,
    scriptpubkey_address: Option<String>,
    scriptpubkey_type: String,

    #[cfg(not(feature = "liquid"))]
    value: u64,

    #[cfg(feature = "liquid")]
    value: Option<u64>,
    #[cfg(feature = "liquid")]
    valuecommitment: Option<String>,
    #[cfg(feature = "liquid")]
    asset: Option<String>,
    #[cfg(feature = "liquid")]
    assetcommitment: Option<String>,
    #[cfg(feature = "liquid")]
    pegout: Option<PegOutRequest>,
}

impl From<(TxOut, &Config)> for TxOutValue {
    fn from((txout, config): (TxOut, &Config)) -> Self {
        #[cfg(not(feature = "liquid"))]
        let value = txout.value;

        #[cfg(feature = "liquid")]
        let value = match txout.value {
            Value::Explicit(value) => Some(value),
            _ => None,
        };
        #[cfg(feature = "liquid")]
        let valuecommitment = match txout.value {
            Value::Confidential(..) => Some(hex::encode(serialize(&txout.value))),
            _ => None,
        };
        #[cfg(feature = "liquid")]
        let asset = match txout.asset {
            Asset::Explicit(value) => Some(value.be_hex_string()),
            _ => None,
        };
        #[cfg(feature = "liquid")]
        let assetcommitment = match txout.asset {
            Asset::Confidential(..) => Some(hex::encode(serialize(&txout.asset))),
            _ => None,
        };

        #[cfg(not(feature = "liquid"))]
        let is_fee = false;
        #[cfg(feature = "liquid")]
        let is_fee = txout.is_fee();

        let script = txout.script_pubkey;
        let script_asm = get_script_asm(&script);
        let script_addr = script_to_address(&script, &config.network_type);

        // TODO should the following something to put inside rust-elements lib?
        let script_type = if is_fee {
            "fee"
        } else if script.is_empty() {
            "empty"
        } else if script.is_op_return() {
            "op_return"
        } else if script.is_p2pk() {
            "p2pk"
        } else if script.is_p2pkh() {
            "p2pkh"
        } else if script.is_p2sh() {
            "p2sh"
        } else if script.is_v0_p2wpkh() {
            "v0_p2wpkh"
        } else if script.is_v0_p2wsh() {
            "v0_p2wsh"
        } else if script.is_provably_unspendable() {
            "provably_unspendable"
        } else {
            "unknown"
        };

        #[cfg(feature = "liquid")]
        let pegout =
            PegOutRequest::parse(&script, &config.parent_network, &config.parent_genesis_hash);

        TxOutValue {
            scriptpubkey: script,
            scriptpubkey_asm: script_asm,
            scriptpubkey_address: script_addr,
            scriptpubkey_type: script_type.to_string(),
            value,
            #[cfg(feature = "liquid")]
            valuecommitment,
            #[cfg(feature = "liquid")]
            asset,
            #[cfg(feature = "liquid")]
            assetcommitment,
            #[cfg(feature = "liquid")]
            pegout,
        }
    }
}

#[derive(Serialize)]
struct UtxoValue {
    txid: Sha256dHash,
    vout: u32,
    status: TransactionStatus,
    #[cfg(not(feature = "liquid"))]
    value: u64,
    #[cfg(feature = "liquid")]
    value: Option<u64>,
    #[cfg(feature = "liquid")]
    valuecommitment: Option<String>,
}
impl From<Utxo> for UtxoValue {
    fn from(utxo: Utxo) -> Self {
        #[cfg(not(feature = "liquid"))]
        let value = utxo.value;

        #[cfg(feature = "liquid")]
        let value = match utxo.value {
            Value::Explicit(value) => Some(value),
            _ => None,
        };
        #[cfg(feature = "liquid")]
        let valuecommitment = match utxo.value {
            Value::Confidential(..) => Some(hex::encode(serialize(&utxo.value))),
            _ => None,
        };

        UtxoValue {
            txid: utxo.txid,
            vout: utxo.vout,
            value,
            status: TransactionStatus::from(utxo.confirmed),
            #[cfg(feature = "liquid")]
            valuecommitment,
        }
    }
}

#[derive(Serialize)]
struct SpendingValue {
    spent: bool,
    txid: Option<Sha256dHash>,
    vin: Option<u32>,
    status: Option<TransactionStatus>,
}
impl From<SpendingInput> for SpendingValue {
    fn from(spend: SpendingInput) -> Self {
        SpendingValue {
            spent: true,
            txid: Some(spend.txid),
            vin: Some(spend.vin),
            status: Some(TransactionStatus::from(spend.confirmed)),
        }
    }
}
impl Default for SpendingValue {
    fn default() -> Self {
        SpendingValue {
            spent: false,
            txid: None,
            vin: None,
            status: None,
        }
    }
}

fn ttl_by_depth(height: Option<usize>, query: &Query) -> u32 {
    height.map_or(TTL_SHORT, |height| {
        if query.chain().best_height() - height >= CONF_FINAL {
            TTL_LONG
        } else {
            TTL_SHORT
        }
    })
}

fn prepare_txs(
    txs: Vec<(Transaction, Option<BlockId>)>,
    query: &Query,
    config: &Config,
) -> Vec<TransactionValue> {
    let prevouts = if config.prevout_enabled {
        let outpoints = txs
            .iter()
            .flat_map(|(tx, _)| {
                tx.input
                    .iter()
                    .filter(|txin| has_prevout(txin))
                    .map(|txin| txin.previous_output)
            })
            .collect();

        query.lookup_txos(&outpoints)
    } else {
        HashMap::new()
    };

    txs.into_iter()
        .map(|(tx, blockid)| TransactionValue::from((tx, blockid, &prevouts, config)))
        .collect()
}

pub fn run_server(config: Arc<Config>, query: Arc<Query>, daemon: Arc<Daemon>) -> Handle {
    let addr = &config.http_addr;
    info!("REST server running on {}", addr);

    let config = Arc::new(config.clone());

    let new_service = move || {
        let query = Arc::clone(&query);
        let config = Arc::clone(&config);
        let daemon = Arc::clone(&daemon);

        service_fn_ok(move |req: Request<Body>| {
            let mut resp = handle_request(req, &query, &config, &daemon).unwrap_or_else(|err| {
                warn!("{:?}", err);
                Response::builder()
                    .status(err.0)
                    .header("Content-Type", "text/plain")
                    .body(Body::from(err.1))
                    .unwrap()
            });
            if let Some(ref origins) = config.cors {
                resp.headers_mut()
                    .insert("Access-Control-Allow-Origin", origins.parse().unwrap());
            }
            resp
        })
    };

    let (tx, rx) = oneshot::channel::<()>();
    let server = Server::bind(&addr)
        .serve(new_service)
        .with_graceful_shutdown(rx)
        .map_err(|e| eprintln!("server error: {}", e));

    Handle {
        tx,
        thread: thread::spawn(move || {
            rt::run(server);
        }),
    }
}

pub struct Handle {
    tx: oneshot::Sender<()>,
    thread: thread::JoinHandle<()>,
}

impl Handle {
    pub fn stop(self) {
        self.tx.send(()).expect("failed to send shutdown signal");
        self.thread.join().expect("REST server failed");
    }
}

fn handle_request(
    req: Request<Body>,
    query: &Query,
    config: &Config,
    daemon: &Daemon,
) -> Result<Response<Body>, HttpError> {
    // TODO it looks hyper does not have routing and query parsing :(
    let uri = req.uri();
    let path: Vec<&str> = uri.path().split('/').skip(1).collect();
    let query_params = match uri.query() {
        Some(value) => form_urlencoded::parse(&value.as_bytes())
            .into_owned()
            .collect::<HashMap<String, String>>(),
        None => HashMap::new(),
    };

    info!("path {:?}", path);
    match (
        req.method(),
        path.get(0),
        path.get(1),
        path.get(2),
        path.get(3),
        path.get(4),
    ) {
        (&Method::GET, Some(&"blocks"), Some(&"tip"), Some(&"hash"), None, None) => http_message(
            StatusCode::OK,
            query.chain().best_hash().be_hex_string(),
            TTL_SHORT,
        ),

        (&Method::GET, Some(&"blocks"), Some(&"tip"), Some(&"height"), None, None) => http_message(
            StatusCode::OK,
            query.chain().best_height().to_string(),
            TTL_SHORT,
        ),

        (&Method::GET, Some(&"blocks"), start_height, None, None, None) => {
            let start_height = start_height.and_then(|height| height.parse::<usize>().ok());
            blocks(&query, start_height)
        }
        (&Method::GET, Some(&"block-height"), Some(height), None, None, None) => {
            let height = height.parse::<usize>()?;
            let header = query
                .chain()
                .header_by_height(height)
                .ok_or_else(|| HttpError::not_found("Block not found".to_string()))?;
            let ttl = ttl_by_depth(Some(height), query);
            http_message(StatusCode::OK, header.hash().be_hex_string(), ttl)
        }
        (&Method::GET, Some(&"block"), Some(hash), None, None, None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let blockhm = query
                .chain()
                .get_block_with_meta(&hash)
                .ok_or_else(|| HttpError::not_found("Block not found".to_string()))?;
            let block_value = BlockValue::from(blockhm);
            json_response(block_value, TTL_LONG)
        }
        (&Method::GET, Some(&"block"), Some(hash), Some(&"status"), None, None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let status = query.chain().get_block_status(&hash);
            let ttl = ttl_by_depth(status.height, query);
            json_response(status, ttl)
        }
        (&Method::GET, Some(&"block"), Some(hash), Some(&"txids"), None, None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let txids = query
                .chain()
                .get_block_txids(&hash)
                .ok_or_else(|| HttpError::not_found("Block not found".to_string()))?;
            json_response(txids, TTL_LONG)
        }
        (&Method::GET, Some(&"block"), Some(hash), Some(&"txs"), start_index, None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let txids = query
                .chain()
                .get_block_txids(&hash)
                .ok_or_else(|| HttpError::not_found("Block not found".to_string()))?;

            let start_index = start_index
                .map_or(0u32, |el| el.parse().unwrap_or(0))
                .max(0u32) as usize;
            if start_index >= txids.len() {
                bail!(HttpError::not_found("start index out of range".to_string()));
            } else if start_index % CHAIN_TXS_PER_PAGE != 0 {
                bail!(HttpError::from(format!(
                    "start index must be a multipication of {}",
                    CHAIN_TXS_PER_PAGE
                )));
            }

            let txs = txids
                .iter()
                .skip(start_index)
                .take(CHAIN_TXS_PER_PAGE)
                .map(|txid| {
                    query
                        .lookup_txn(&txid)
                        // FIXME: set blockid correctly, only when the block is non-orphaned
                        //        alternatively, don't set "status" at all?
                        .map(|tx| (tx, None))
                        .ok_or_else(|| "missing tx".to_string())
                })
                .collect::<Result<Vec<(Transaction, Option<BlockId>)>, _>>()?;

            json_response(prepare_txs(txs, query, config), TTL_LONG)
        }
        (&Method::GET, Some(script_type @ &"address"), Some(script_str), None, None, None)
        | (&Method::GET, Some(script_type @ &"scripthash"), Some(script_str), None, None, None) => {
            let script_hash = to_scripthash(script_type, script_str, &config.network_type)?;
            let stats = query.stats(&script_hash[..]);
            json_response(
                json!({
                    *script_type: script_str,
                    "chain_stats": stats.0,
                    "mempool_stats": stats.1,
                }),
                TTL_SHORT,
            )
        }
        (
            &Method::GET,
            Some(script_type @ &"address"),
            Some(script_str),
            Some(&"txs"),
            None,
            None,
        )
        | (
            &Method::GET,
            Some(script_type @ &"scripthash"),
            Some(script_str),
            Some(&"txs"),
            None,
            None,
        ) => {
            let script_hash = to_scripthash(script_type, script_str, &config.network_type)?;

            let mut txs = vec![];

            txs.extend(
                query
                    .mempool()
                    .history(&script_hash[..], MAX_MEMPOOL_TXS)
                    .into_iter()
                    .map(|tx| (tx, None)),
            );

            txs.extend(
                query
                    .chain()
                    .history(&script_hash[..], None, CHAIN_TXS_PER_PAGE)
                    .into_iter(),
            );

            json_response(prepare_txs(txs, query, config), TTL_SHORT)
        }

        (
            &Method::GET,
            Some(script_type @ &"address"),
            Some(script_str),
            Some(&"txs"),
            Some(&"chain"),
            last_seen_txid,
        )
        | (
            &Method::GET,
            Some(script_type @ &"scripthash"),
            Some(script_str),
            Some(&"txs"),
            Some(&"chain"),
            last_seen_txid,
        ) => {
            let script_hash = to_scripthash(script_type, script_str, &config.network_type)?;
            let last_seen_txid = last_seen_txid.and_then(|txid| Sha256dHash::from_hex(txid).ok());

            let txs = query.chain().history(
                &script_hash[..],
                last_seen_txid.as_ref(),
                CHAIN_TXS_PER_PAGE,
            );

            json_response(prepare_txs(txs, query, config), TTL_SHORT)
        }
        (
            &Method::GET,
            Some(script_type @ &"address"),
            Some(script_str),
            Some(&"txs"),
            Some(&"mempool"),
            None,
        )
        | (
            &Method::GET,
            Some(script_type @ &"scripthash"),
            Some(script_str),
            Some(&"txs"),
            Some(&"mempool"),
            None,
        ) => {
            let script_hash = to_scripthash(script_type, script_str, &config.network_type)?;

            let txs = query
                .mempool()
                .history(&script_hash[..], MAX_MEMPOOL_TXS)
                .into_iter()
                .map(|tx| (tx, None))
                .collect();

            json_response(prepare_txs(txs, query, config), TTL_SHORT)
        }

        (
            &Method::GET,
            Some(script_type @ &"address"),
            Some(script_str),
            Some(&"utxo"),
            None,
            None,
        )
        | (
            &Method::GET,
            Some(script_type @ &"scripthash"),
            Some(script_str),
            Some(&"utxo"),
            None,
            None,
        ) => {
            let script_hash = to_scripthash(script_type, script_str, &config.network_type)?;
            let utxos: Vec<UtxoValue> = query
                .utxo(&script_hash[..])
                .into_iter()
                .map(UtxoValue::from)
                .collect();
            // XXX paging?
            json_response(utxos, TTL_SHORT)
        }
        (&Method::GET, Some(&"tx"), Some(hash), None, None, None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let tx = query
                .lookup_txn(&hash)
                .ok_or_else(|| HttpError::not_found("Transaction not found".to_string()))?;
            let confirmation = query.chain().tx_confirming_block(&hash);
            let ttl = ttl_by_depth(confirmation.as_ref().map(|c| c.height), query);

            let tx = prepare_txs(vec![(tx, confirmation)], query, config).remove(0);

            json_response(tx, ttl)
        }
        (&Method::GET, Some(&"tx"), Some(hash), Some(&"hex"), None, None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let rawtx = query
                .lookup_raw_txn(&hash)
                .ok_or_else(|| HttpError::not_found("Transaction not found".to_string()))?;
            let ttl = ttl_by_depth(query.get_tx_status(&hash).block_height, query);
            http_message(StatusCode::OK, hex::encode(rawtx), ttl)
        }
        (&Method::GET, Some(&"tx"), Some(hash), Some(&"status"), None, None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let status = query.get_tx_status(&hash);
            let ttl = ttl_by_depth(status.block_height, query);
            json_response(status, ttl)
        }
        // TODO: implement merkle proof
        /*
        (&Method::GET, Some(&"tx"), Some(hash), Some(&"merkle-proof"), None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let status = query.get_tx_status(&hash);
            if !status.confirmed {
                bail!("Transaction is unconfirmed".to_string())
            };
            let proof = query.get_merkle_proof(&hash, &status.block_hash.unwrap())?;
            let ttl = ttl_by_depth(status.block_height, query);
            json_response(
                json!({ "block_height": status.block_height, "merkle": proof.0, "pos": proof.1 }),
                ttl,
            )
        }
        */
        (&Method::GET, Some(&"tx"), Some(hash), Some(&"outspend"), Some(index), None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let outpoint = OutPoint {
                txid: hash,
                vout: index.parse::<u32>()?,
            };
            let spend = query
                .lookup_spend(&outpoint)
                .map_or_else(SpendingValue::default, SpendingValue::from);
            let ttl = ttl_by_depth(
                spend
                    .status
                    .as_ref()
                    .and_then(|ref status| status.block_height),
                query,
            );
            json_response(spend, ttl)
        }
        (&Method::GET, Some(&"tx"), Some(hash), Some(&"outspends"), None, None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let tx = query
                .lookup_txn(&hash)
                .ok_or_else(|| HttpError::not_found("Transaction not found".to_string()))?;
            let spends: Vec<SpendingValue> = query
                .lookup_tx_spends(tx)
                .into_iter()
                .map(|spend| {
                    spend.map_or_else(
                        || SpendingValue::default(),
                        |spend| SpendingValue::from(spend),
                    )
                })
                .collect();
            // @TODO long ttl if all outputs are either spent long ago or unspendable
            json_response(spends, TTL_SHORT)
        }
        (&Method::POST, Some(&"tx"), None, None, None, None) => {
            // FIXME read txhex from post body
            let txhex = query_params
                .get("txhex")
                .ok_or_else(|| HttpError::from("Missing txhex".to_string()))?;
            let txid = daemon
                .broadcast_raw(&txhex)
                .map_err(|err| HttpError::from(err.description().to_string()))?;
            http_message(StatusCode::OK, hex::encode(serialize(&txid)), 0)
        }

        _ => Err(HttpError::not_found(format!(
            "endpoint does not exist {:?}",
            uri.path()
        ))),
    }
}

fn http_message(
    status: StatusCode,
    message: String,
    ttl: u32,
) -> Result<Response<Body>, HttpError> {
    Ok(Response::builder()
        .status(status)
        .header("Content-Type", "text/plain")
        .header("Cache-Control", format!("public, max-age={:}", ttl))
        .body(Body::from(message))
        .unwrap())
}

fn json_response<T: Serialize>(value: T, ttl: u32) -> Result<Response<Body>, HttpError> {
    let value = serde_json::to_string(&value)?;
    Ok(Response::builder()
        .header("Content-Type", "application/json")
        .header("Cache-Control", format!("public, max-age={:}", ttl))
        .body(Body::from(value))
        .unwrap())
}

fn blocks(query: &Query, start_height: Option<usize>) -> Result<Response<Body>, HttpError> {
    let mut values = Vec::new();
    let mut current_hash = match start_height {
        Some(height) => query
            .chain()
            .header_by_height(height)
            .ok_or_else(|| HttpError::not_found("Block not found".to_string()))?
            .hash()
            .clone(),
        None => query.chain().best_header().hash().clone(),
    };

    let zero = [0u8; 32];
    for _ in 0..BLOCK_LIMIT {
        let blockhm = query
            .chain()
            .get_block_with_meta(&current_hash)
            .ok_or_else(|| HttpError::not_found("Block not found".to_string()))?;
        current_hash = blockhm.header_entry.header().prev_blockhash.clone();

        #[allow(unused_mut)]
        let mut value = BlockValue::from(blockhm);

        #[cfg(feature = "liquid")]
        {
            // exclude proof in block list view
            value.proof = None;
        }
        values.push(value);

        if &current_hash[..] == &zero[..] {
            break;
        }
    }
    json_response(values, TTL_SHORT)
}

fn to_scripthash(
    script_type: &str,
    script_str: &str,
    network: &Network,
) -> Result<FullHash, HttpError> {
    match script_type {
        "address" => address_to_scripthash(script_str, network),
        "scripthash" => Ok(full_hash(&hex::decode(script_str)?)),
        _ => bail!("Invalid script type".to_string()),
    }
}

fn address_to_scripthash(addr: &str, network: &Network) -> Result<FullHash, HttpError> {
    let addr = Address::from_str(addr)?;

    #[cfg(not(feature = "liquid"))]
    let regtest_net = Network::Regtest;
    #[cfg(feature = "liquid")]
    let regtest_net = Network::LiquidRegtest;

    if addr.network != *network && !(addr.network == Network::Testnet && *network == regtest_net) {
        bail!(HttpError::from("Address on invalid network".to_string()))
    }
    Ok(compute_script_hash(&addr.script_pubkey()))
}

#[derive(Debug)]
struct HttpError(StatusCode, String);

impl HttpError {
    fn not_found(msg: String) -> Self {
        HttpError(StatusCode::NOT_FOUND, msg)
    }
    fn generic() -> Self {
        HttpError::from("We encountered an error. Please try again later.".to_string())
    }
}

impl From<String> for HttpError {
    fn from(msg: String) -> Self {
        HttpError(StatusCode::BAD_REQUEST, msg)
    }
}
impl From<ParseIntError> for HttpError {
    fn from(_e: ParseIntError) -> Self {
        //HttpError::from(e.description().to_string())
        HttpError::from("Invalid number".to_string())
    }
}
impl From<HexError> for HttpError {
    fn from(_e: HexError) -> Self {
        //HttpError::from(e.description().to_string())
        HttpError::from("Invalid hex string".to_string())
    }
}
impl From<FromHexError> for HttpError {
    fn from(_e: FromHexError) -> Self {
        //HttpError::from(e.description().to_string())
        HttpError::from("Invalid hex string".to_string())
    }
}
impl From<errors::Error> for HttpError {
    fn from(e: errors::Error) -> Self {
        warn!("errors::Error: {:?}", e);
        match e.description().to_string().as_ref() {
            "getblock RPC error: {\"code\":-5,\"message\":\"Block not found\"}" => {
                HttpError::not_found("Block not found".to_string())
            }
            _ => HttpError::generic(),
        }
    }
}
impl From<serde_json::Error> for HttpError {
    fn from(_e: serde_json::Error) -> Self {
        //HttpError::from(e.description().to_string())
        HttpError::generic()
    }
}
impl From<encode::Error> for HttpError {
    fn from(_e: encode::Error) -> Self {
        //HttpError::from(e.description().to_string())
        HttpError::generic()
    }
}

#[cfg(test)]
mod tests {
    use crate::rest::HttpError;
    use serde_json::Value;
    use std::collections::HashMap;

    #[test]
    fn test_parse_query_param() {
        let mut query_params = HashMap::new();

        query_params.insert("limit", "10");
        let limit = query_params
            .get("limit")
            .map_or(10u32, |el| el.parse().unwrap_or(10u32))
            .min(30u32);
        assert_eq!(10, limit);

        query_params.insert("limit", "100");
        let limit = query_params
            .get("limit")
            .map_or(10u32, |el| el.parse().unwrap_or(10u32))
            .min(30u32);
        assert_eq!(30, limit);

        query_params.insert("limit", "5");
        let limit = query_params
            .get("limit")
            .map_or(10u32, |el| el.parse().unwrap_or(10u32))
            .min(30u32);
        assert_eq!(5, limit);

        query_params.insert("limit", "aaa");
        let limit = query_params
            .get("limit")
            .map_or(10u32, |el| el.parse().unwrap_or(10u32))
            .min(30u32);
        assert_eq!(10, limit);

        query_params.remove("limit");
        let limit = query_params
            .get("limit")
            .map_or(10u32, |el| el.parse().unwrap_or(10u32))
            .min(30u32);
        assert_eq!(10, limit);
    }

    #[test]
    fn test_parse_value_param() {
        let v: Value = json!({ "confirmations": 10 });

        let confirmations = v
            .get("confirmations")
            .and_then(|el| el.as_u64())
            .ok_or(HttpError::from(
                "confirmations absent or not a u64".to_string(),
            ))
            .unwrap();

        assert_eq!(10, confirmations);

        let err = v
            .get("notexist")
            .and_then(|el| el.as_u64())
            .ok_or(HttpError::from("notexist absent or not a u64".to_string()));

        assert!(err.is_err());
    }
}
