use bitcoin::consensus::encode::{self, serialize};
use bitcoin::network::constants::Network;
use bitcoin::util::address::Address;
use bitcoin::util::hash::{HexError, Sha256dHash};
use bitcoin::{BitcoinHash, Script};
use bitcoin::{Transaction, TxIn, TxOut};
use config::Config;
use errors;
use hex::{self, FromHexError};
use hyper::rt::{self, Future};
use hyper::service::service_fn_ok;
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use index::compute_script_hash;
use mempool::MEMPOOL_HEIGHT;
use query::{FundingOutput, Query, SpendingInput, TxnHeight};
use serde::Serialize;
use serde_json;
use std::collections::BTreeMap;
use std::num::ParseIntError;
use std::str::FromStr;
use std::sync::Arc;
use std::thread;
use util::{get_script_asm, script_to_address, BlockHeaderMeta, FullHash, TransactionStatus};

const TX_LIMIT: usize = 25;
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
    bits: u32,
    nonce: u32,
    tx_count: u32,
    size: u32,
    weight: u32,
    merkle_root: String,
    previousblockhash: Option<String>,
}

impl From<BlockHeaderMeta> for BlockValue {
    fn from(blockhm: BlockHeaderMeta) -> Self {
        let header = blockhm.header_entry.header();
        BlockValue {
            id: header.bitcoin_hash().be_hex_string(),
            height: blockhm.header_entry.height() as u32,
            version: header.version,
            timestamp: header.time,
            bits: header.bits,
            nonce: header.nonce,
            tx_count: blockhm.meta.tx_count,
            size: blockhm.meta.size,
            weight: blockhm.meta.weight,
            merkle_root: header.merkle_root.be_hex_string(),
            previousblockhash: if &header.prev_blockhash != &Sha256dHash::default() {
                Some(header.prev_blockhash.be_hex_string())
            } else {
                None
            },
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

impl From<Transaction> for TransactionValue {
    fn from(tx: Transaction) -> Self {
        let vin = tx
            .input
            .iter()
            .map(|el| TxInValue::from(el.clone()))
            .collect();
        let vout = tx
            .output
            .iter()
            .map(|el| TxOutValue::from(el.clone()))
            .collect();
        let bytes = serialize(&tx);

        TransactionValue {
            txid: tx.txid(),
            version: tx.version,
            locktime: tx.lock_time,
            vin,
            vout,
            size: bytes.len() as u32,
            weight: tx.get_weight() as u32,
            fee: None, // added later
            status: None,
        }
    }
}

impl From<TxnHeight> for TransactionValue {
    fn from(t: TxnHeight) -> Self {
        let TxnHeight {
            txn,
            height,
            blockhash,
        } = t;
        let mut value = TransactionValue::from(txn);
        value.status = Some(if height != MEMPOOL_HEIGHT {
            TransactionStatus {
                confirmed: true,
                block_height: Some(height as usize),
                block_hash: Some(blockhash),
            }
        } else {
            TransactionStatus::unconfirmed()
        });
        value
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
}

impl From<TxIn> for TxInValue {
    fn from(txin: TxIn) -> Self {
        let script = txin.script_sig;
        let witness = if txin.witness.len() > 0 {
            Some(txin.witness.iter().map(|w| hex::encode(w)).collect())
        } else {
            None
        };

        TxInValue {
            txid: txin.previous_output.txid,
            vout: txin.previous_output.vout,
            prevout: None, // added later
            scriptsig_asm: get_script_asm(&script),
            scriptsig: script,
            witness: witness,
            is_coinbase: txin.previous_output.is_null(),
            sequence: txin.sequence,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct TxOutValue {
    scriptpubkey: Script,
    scriptpubkey_asm: String,
    value: u64,
    scriptpubkey_address: Option<String>,
    scriptpubkey_type: String,
}

impl From<TxOut> for TxOutValue {
    fn from(txout: TxOut) -> Self {
        let value = txout.value;
        let script = txout.script_pubkey;
        let script_asm = get_script_asm(&script);

        // TODO should the following something to put inside rust-elements lib?
        let mut script_type = "";
        if script.is_empty() {
            script_type = "empty";
        } else if script.is_op_return() {
            script_type = "op_return";
        } else if script.is_p2pk() {
            script_type = "p2pk";
        } else if script.is_p2pkh() {
            script_type = "p2pkh";
        } else if script.is_p2sh() {
            script_type = "p2sh";
        } else if script.is_v0_p2wpkh() {
            script_type = "v0_p2wpkh";
        } else if script.is_v0_p2wsh() {
            script_type = "v0_p2wsh";
        } else if script.is_provably_unspendable() {
            script_type = "provably_unspendable";
        }

        TxOutValue {
            scriptpubkey: script,
            scriptpubkey_asm: script_asm,
            scriptpubkey_address: None, // added later
            scriptpubkey_type: script_type.to_string(),
            value,
        }
    }
}

#[derive(Serialize)]
struct UtxoValue {
    txid: Sha256dHash,
    vout: u32,
    value: u64,
    status: TransactionStatus,
}
impl From<FundingOutput> for UtxoValue {
    fn from(out: FundingOutput) -> Self {
        let FundingOutput {
            txn,
            txn_id,
            output_index,
            value,
            ..
        } = out;
        let TxnHeight {
            height, blockhash, ..
        } = txn.unwrap(); // we should never get a FundingOutput without a txn here

        UtxoValue {
            txid: txn_id,
            vout: output_index as u32,
            value: value,
            status: if height != MEMPOOL_HEIGHT {
                TransactionStatus {
                    confirmed: true,
                    block_height: Some(height as usize),
                    block_hash: Some(blockhash),
                }
            } else {
                TransactionStatus::unconfirmed()
            },
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
        let SpendingInput {
            txn,
            txn_id,
            input_index,
            ..
        } = spend;
        let TxnHeight {
            height, blockhash, ..
        } = txn.unwrap(); // we should never get a SpendingInput without a txn here

        SpendingValue {
            spent: true,
            txid: Some(txn_id),
            vin: Some(input_index as u32),
            status: Some(if height != MEMPOOL_HEIGHT {
                TransactionStatus {
                    confirmed: true,
                    block_height: Some(height as usize),
                    block_hash: Some(blockhash),
                }
            } else {
                TransactionStatus::unconfirmed()
            }),
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
        if query.get_best_height() - height >= CONF_FINAL {
            TTL_LONG
        } else {
            TTL_SHORT
        }
    })
}

fn attach_tx_data(tx: TransactionValue, config: &Config, query: &Arc<Query>) -> TransactionValue {
    let mut txs = vec![tx];
    attach_txs_data(&mut txs, config, query);
    txs.remove(0)
}

fn attach_txs_data(txs: &mut Vec<TransactionValue>, config: &Config, query: &Arc<Query>) {
    {
        // a map of prev txids/vouts to lookup, with a reference to the "next in" that spends them
        let mut lookups: BTreeMap<Sha256dHash, Vec<(u32, &mut TxInValue)>> = BTreeMap::new();
        // using BTreeMap ensures the txid keys are in order. querying the db with keys in order leverage memory
        // locality from empirical test up to 2 or 3 times faster

        for mut tx in txs.iter_mut() {
            // collect lookups
            if config.prevout_enabled {
                for mut vin in tx.vin.iter_mut() {
                    if !vin.is_coinbase {
                        lookups
                            .entry(vin.txid)
                            .or_insert(vec![])
                            .push((vin.vout, vin));
                    }
                }
            }
            // attach encoded address (should ideally happen in TxOutValue::from(), but it cannot
            // easily access the network)
            for mut vout in tx.vout.iter_mut() {
                vout.scriptpubkey_address =
                    script_to_address(&vout.scriptpubkey, &config.network_type);
            }
        }

        // fetch prevtxs and attach prevouts to nextins
        if config.prevout_enabled {
            for (prev_txid, prev_vouts) in lookups {
                let prevtx = query.load_txn(&prev_txid, None).unwrap();
                for (prev_out_idx, ref mut nextin) in prev_vouts {
                    let mut prevout =
                        TxOutValue::from(prevtx.output[prev_out_idx as usize].clone());
                    prevout.scriptpubkey_address =
                        script_to_address(&prevout.scriptpubkey, &config.network_type);
                    nextin.prevout = Some(prevout);
                }
            }
        }
    }

    // attach tx fee
    if config.prevout_enabled {
        for mut tx in txs.iter_mut() {
            if tx.vin.iter().any(|vin| vin.prevout.is_none()) {
                continue;
            }

            let total_in: u64 = tx
                .vin
                .iter()
                .map(|vin| vin.clone().prevout.unwrap().value)
                .sum();
            let total_out: u64 = tx.vout.iter().map(|vout| vout.value).sum();
            tx.fee = Some(total_in - total_out);
        }
    }
}

pub fn run_server(config: &Config, query: Arc<Query>) {
    let addr = &config.http_addr;
    info!("REST server running on {}", addr);

    let config = Arc::new(config.clone());

    let new_service = move || {
        let query = query.clone();
        let config = config.clone();

        service_fn_ok(
            move |req: Request<Body>| match handle_request(req, &query, &config) {
                Ok(response) => response,
                Err(e) => {
                    warn!("{:?}", e);
                    Response::builder()
                        .status(e.0)
                        .header("Content-Type", "text/plain")
                        .body(Body::from(e.1))
                        .unwrap()
                }
            },
        )
    };

    let server = Server::bind(&addr)
        .serve(new_service)
        .map_err(|e| eprintln!("server error: {}", e));

    thread::spawn(move || {
        rt::run(server);
    });
}

fn handle_request(
    req: Request<Body>,
    query: &Arc<Query>,
    config: &Config,
) -> Result<Response<Body>, HttpError> {
    // TODO it looks hyper does not have routing and query parsing :(
    let uri = req.uri();
    let path: Vec<&str> = uri.path().split('/').skip(1).collect();
    info!("path {:?}", path);
    match (
        req.method(),
        path.get(0),
        path.get(1),
        path.get(2),
        path.get(3),
    ) {
        (&Method::GET, Some(&"blocks"), Some(&"tip"), Some(&"hash"), None) => http_message(
            StatusCode::OK,
            query.get_best_header_hash().be_hex_string(),
            TTL_SHORT,
        ),

        (&Method::GET, Some(&"blocks"), Some(&"tip"), Some(&"height"), None) => http_message(
            StatusCode::OK,
            query.get_best_height().to_string(),
            TTL_SHORT,
        ),

        (&Method::GET, Some(&"blocks"), start_height, None, None) => {
            let start_height = start_height.and_then(|height| height.parse::<usize>().ok());
            blocks(&query, start_height)
        }
        (&Method::GET, Some(&"block-height"), Some(height), None, None) => {
            let height = height.parse::<usize>()?;
            let headers = query.get_headers(&[height]);
            let header = headers
                .get(0)
                .ok_or_else(|| HttpError::not_found("Block not found".to_string()))?;
            let ttl = ttl_by_depth(Some(height), query);
            http_message(StatusCode::OK, header.hash().be_hex_string(), ttl)
        }
        (&Method::GET, Some(&"block"), Some(hash), None, None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let blockhm = query.get_block_header_with_meta(&hash)?;
            let block_value = BlockValue::from(blockhm);
            json_response(block_value, TTL_LONG)
        }
        (&Method::GET, Some(&"block"), Some(hash), Some(&"status"), None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let status = query.get_block_status(&hash);
            let ttl = ttl_by_depth(status.height, query);
            json_response(status, ttl)
        }
        (&Method::GET, Some(&"block"), Some(hash), Some(&"txids"), None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let txids = query
                .get_block_txids(&hash)
                .map_err(|_| HttpError::not_found("Block not found".to_string()))?;
            json_response(txids, TTL_LONG)
        }
        (&Method::GET, Some(&"block"), Some(hash), Some(&"txs"), start_index) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let txids = query
                .get_block_txids(&hash)
                .map_err(|_| HttpError::not_found("Block not found".to_string()))?;

            let start_index = start_index
                .map_or(0u32, |el| el.parse().unwrap_or(0))
                .max(0u32) as usize;
            if start_index >= txids.len() {
                bail!(HttpError::not_found("start index out of range".to_string()));
            } else if start_index % TX_LIMIT != 0 {
                bail!(HttpError::from(format!(
                    "start index must be a multipication of {}",
                    TX_LIMIT
                )));
            }

            let mut txs = txids
                .iter()
                .skip(start_index)
                .take(TX_LIMIT)
                .map(|txid| {
                    query
                        .load_txn(&txid, Some(&hash))
                        .map(TransactionValue::from)
                }).collect::<Result<Vec<TransactionValue>, _>>()?;
            attach_txs_data(&mut txs, config, query);
            json_response(txs, TTL_LONG)
        }
        (&Method::GET, Some(&"address"), Some(address), None, None) => {
            // @TODO create new AddressStatsValue struct?
            let script_hash = address_to_scripthash(address, &config.network_type)?;
            match query.status(&script_hash[..]) {
                Ok(status) => json_response(
                    json!({
                    "address": address,
                    "tx_count": status.history().len(),
                    "confirmed_balance": status.confirmed_balance(),
                    "mempool_balance": status.mempool_balance(),
                    "total_received": status.total_received(),
                }),
                    TTL_SHORT,
                ),

                // if the address has too many txs, just return the address with no additional info (but no error)
                Err(errors::Error(errors::ErrorKind::Msg(ref msg), _))
                    if *msg == "Too many txs".to_string() =>
                {
                    json_response(json!({ "address": address }), TTL_SHORT)
                }

                Err(err) => bail!(err),
            }
        }
        (&Method::GET, Some(&"address"), Some(address), Some(&"txs"), start_index) => {
            let start_index = start_index
                .map_or(0u32, |el| el.parse().unwrap_or(0))
                .max(0u32) as usize;

            let script_hash = address_to_scripthash(address, &config.network_type)?;
            let status = query.status(&script_hash[..])?;
            let txs = status.history_txs();

            if txs.len() == 0 {
                return json_response(json!([]), TTL_SHORT);
            } else if start_index >= txs.len() {
                bail!(HttpError::not_found("start index out of range".to_string()));
            } else if start_index % TX_LIMIT != 0 {
                bail!(HttpError::from(format!(
                    "start index must be a multipication of {}",
                    TX_LIMIT
                )));
            }

            let mut txs = txs
                .iter()
                .skip(start_index)
                .take(TX_LIMIT)
                .map(|t| TransactionValue::from((*t).clone()))
                .collect();
            attach_txs_data(&mut txs, config, query);

            json_response(txs, TTL_SHORT)
        }
        (&Method::GET, Some(&"address"), Some(address), Some(&"utxo"), None) => {
            let script_hash = address_to_scripthash(address, &config.network_type)?;
            let status = query.status(&script_hash[..])?;
            let utxos: Vec<UtxoValue> = status
                .unspent()
                .into_iter()
                .map(|o| UtxoValue::from(o.clone()))
                .collect();
            // @XXX no paging, but query.status() is limited to 30 funding txs
            json_response(utxos, TTL_SHORT)
        }
        (&Method::GET, Some(&"tx"), Some(hash), None, None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let transaction = query
                .load_txn(&hash, None)
                .map_err(|_| HttpError::not_found("Transaction not found".to_string()))?;
            let status = query.get_tx_status(&hash)?;
            let ttl = ttl_by_depth(status.block_height, query);

            let mut value = TransactionValue::from(transaction);
            value.status = Some(status);
            let value = attach_tx_data(value, config, query);
            json_response(value, ttl)
        }
        (&Method::GET, Some(&"tx"), Some(hash), Some(&"hex"), None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let rawtx = query
                .load_raw_txn(&hash, None)
                .map_err(|_| HttpError::not_found("Transaction not found".to_string()))?;
            let ttl = ttl_by_depth(query.get_tx_status(&hash)?.block_height, query);
            http_message(StatusCode::OK, hex::encode(rawtx), ttl)
        }
        (&Method::GET, Some(&"tx"), Some(hash), Some(&"status"), None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let status = query.get_tx_status(&hash)?;
            let ttl = ttl_by_depth(status.block_height, query);
            json_response(status, ttl)
        }
        (&Method::GET, Some(&"tx"), Some(hash), Some(&"merkle-proof"), None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let status = query.get_tx_status(&hash)?;
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
        (&Method::GET, Some(&"tx"), Some(hash), Some(&"outspend"), Some(index)) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let outpoint = (hash, index.parse::<usize>()?);
            let spend = query.find_spending_by_outpoint(outpoint)?.map_or_else(
                || SpendingValue::default(),
                |spend| SpendingValue::from(spend),
            );
            let ttl = ttl_by_depth(
                spend
                    .status
                    .as_ref()
                    .and_then(|ref status| status.block_height),
                query,
            );
            json_response(spend, ttl)
        }
        (&Method::GET, Some(&"tx"), Some(hash), Some(&"outspends"), None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let tx = query
                .load_txn(&hash, None)
                .map_err(|_| HttpError::not_found("Transaction not found".to_string()))?;
            let spends: Vec<SpendingValue> = query
                .find_spending_for_funding_tx(tx)?
                .into_iter()
                .map(|spend| {
                    spend.map_or_else(
                        || SpendingValue::default(),
                        |spend| SpendingValue::from(spend),
                    )
                }).collect();
            // @TODO long ttl if all outputs are either spent long ago or unspendable
            json_response(spends, TTL_SHORT)
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

fn blocks(query: &Arc<Query>, start_height: Option<usize>) -> Result<Response<Body>, HttpError> {
    let mut values = Vec::new();
    let mut current_hash = match start_height {
        Some(height) => query
            .get_headers(&[height])
            .get(0)
            .ok_or(HttpError::not_found("Block not found".to_string()))?
            .hash()
            .clone(),
        None => query.get_best_header()?.hash().clone(),
    };

    let zero = [0u8; 32];
    for _ in 0..BLOCK_LIMIT {
        let blockhm = query.get_block_header_with_meta(&current_hash)?;
        current_hash = blockhm.header_entry.header().prev_blockhash.clone();
        values.push(BlockValue::from(blockhm));

        if &current_hash[..] == &zero[..] {
            break;
        }
    }
    json_response(values, TTL_SHORT)
}

fn address_to_scripthash(addr: &str, network: &Network) -> Result<FullHash, HttpError> {
    let addr = Address::from_str(addr)?;
    if addr.network != *network
        && !(addr.network == Network::Testnet && *network == Network::Regtest)
    {
        bail!(HttpError::from("Address on invalid network".to_string()))
    }
    Ok(compute_script_hash(&addr.script_pubkey().into_bytes()))
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
            "Too many txs" => HttpError(
                StatusCode::TOO_MANY_REQUESTS,
                "Sorry! Addresses with a large number of transactions aren\'t currently supported."
                    .to_string(),
            ),
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
    use rest::HttpError;
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
            )).unwrap();

        assert_eq!(10, confirmations);

        let err = v
            .get("notexist")
            .and_then(|el| el.as_u64())
            .ok_or(HttpError::from("notexist absent or not a u64".to_string()));

        assert!(err.is_err());
    }
}
