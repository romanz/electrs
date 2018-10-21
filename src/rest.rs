use bitcoin::util::hash::{Sha256dHash,HexError};
use bitcoin::network::serialize::serialize;
use bitcoin::{Script,network,BitcoinHash};
use config::Config;
use bitcoin::{TxIn,TxOut,OutPoint,Transaction};
use bitcoin::util::address::Address;
use errors;
use hex::{self, FromHexError};
use hyper::{Body, Response, Server, Method, Request, StatusCode};
use hyper::service::service_fn_ok;
use hyper::rt::{self, Future};
use query::{Query, TxnHeight};
use serde_json;
use serde::Serialize;
use std::collections::BTreeMap;
use std::error::Error;
use std::num::ParseIntError;
use std::str::FromStr;
use std::thread;
use std::sync::Arc;
use bitcoin::network::constants::Network;
use util::{FullHash, BlockHeaderMeta, TransactionStatus, script_to_address};
use index::compute_script_hash;

const TX_LIMIT: usize = 50;
const BLOCK_LIMIT: usize = 10;

#[derive(Serialize, Deserialize)]
struct BlockValue {
    id: String,
    height: u32,
    timestamp: u32,
    tx_count: u32,
    size: u32,
    weight: u32,
    previousblockhash: Option<String>,
}

impl From<BlockHeaderMeta> for BlockValue {
    fn from(blockhm: BlockHeaderMeta) -> Self {
        let header = blockhm.header_entry.header();
        BlockValue {
            id: header.bitcoin_hash().be_hex_string(),
            height: blockhm.header_entry.height() as u32,
            timestamp: header.time,
            tx_count: blockhm.meta.tx_count,
            size: blockhm.meta.size,
            weight: blockhm.meta.weight,
            previousblockhash: if &header.prev_blockhash != &Sha256dHash::default() { Some(header.prev_blockhash.be_hex_string()) }
                               else { None },
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
        let vin = tx.input.iter().map(|el| TxInValue::from(el.clone())).collect();
        let vout = tx.output.iter().map(|el| TxOutValue::from(el.clone())).collect();
        let bytes = serialize(&tx).unwrap();

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
        let TxnHeight { txn, height, blockhash } = t;
        let mut value = TransactionValue::from(txn);
        value.status = Some(if height != 0 {
          TransactionStatus { confirmed: true, block_height: Some(height), block_hash: Some(blockhash) }
        } else {
          TransactionStatus::unconfirmed()
        });
        value
    }
}


#[derive(Serialize, Deserialize, Clone)]
struct TxInValue {
    outpoint: OutPoint,
    prevout: Option<TxOutValue>,
    scriptsig_hex: Script,
    scriptsig_asm: String,
    witness: Option<Vec<String>>,
    is_coinbase: bool,
}

impl From<TxIn> for TxInValue {
    fn from(txin: TxIn) -> Self {
        let script = txin.script_sig;
        let script_asm = format!("{:?}",script);
        let witness = if txin.witness.len() > 0 { Some(txin.witness.iter().map(|w| hex::encode(w)).collect()) } else { None };

        TxInValue {
            outpoint: txin.previous_output,
            prevout: None, // added later
            scriptsig_asm: (&script_asm[7..script_asm.len()-1]).to_string(),
            scriptsig_hex: script,
            witness: witness,
            is_coinbase: txin.previous_output.is_null(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct TxOutValue {
    scriptpubkey_hex: Script,
    scriptpubkey_asm: String,
    value: u64,
    scriptpubkey_address: Option<String>,
    scriptpubkey_type: String,
}

impl From<TxOut> for TxOutValue {
    fn from(txout: TxOut) -> Self {
        let value = txout.value;
        let script = txout.script_pubkey;
        let script_asm = format!("{:?}",script);

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
            scriptpubkey_hex: script,
            scriptpubkey_asm: (&script_asm[7..script_asm.len()-1]).to_string(),
            scriptpubkey_address: None, // added later
            value,
            scriptpubkey_type: script_type.to_string(),
        }
    }
}


fn attach_tx_data(tx: TransactionValue, network: &Network, query: &Arc<Query>) -> TransactionValue {
    let mut txs = vec![tx];
    attach_txs_data(&mut txs, network, query);
    txs.remove(0)
}

fn attach_txs_data(txs: &mut Vec<TransactionValue>, network: &Network, query: &Arc<Query>) {
    {
        // a map of prev txids/vouts to lookup, with a reference to the "next in" that spends them
        let mut lookups: BTreeMap<Sha256dHash, Vec<(u32, &mut TxInValue)>> = BTreeMap::new();
        // using BTreeMap ensures the txid keys are in order. querying the db with keys in order leverage memory
        // locality from empirical test up to 2 or 3 times faster

        for mut tx in txs.iter_mut() {
            // collect lookups
            for mut vin in tx.vin.iter_mut() {
                if !vin.is_coinbase {
                    lookups.entry(vin.outpoint.txid).or_insert(vec![]).push((vin.outpoint.vout, vin));
                }
            }
            // attach encoded address (should ideally happen in TxOutValue::from(), but it cannot
            // easily access the network)
            for mut vout in tx.vout.iter_mut() {
                vout.scriptpubkey_address = script_to_address(&vout.scriptpubkey_hex, &network);
            }
        }

        // fetch prevtxs and attach prevouts to nextins
        for (prev_txid, prev_vouts) in lookups {
            let prevtx = query.tx_get(&prev_txid).unwrap();
            for (prev_out_idx, ref mut nextin) in prev_vouts {
                let mut prevout = TxOutValue::from(prevtx.output[prev_out_idx as usize].clone());
                prevout.scriptpubkey_address = script_to_address(&prevout.scriptpubkey_hex, &network);
                nextin.prevout = Some(prevout);
            }
        }
    }

    // attach tx fee
    for mut tx in txs.iter_mut() {
        if tx.vin.iter().any(|vin| vin.prevout.is_none()) { continue; }

        let total_in: u64 = tx.vin.iter().map(|vin| vin.clone().prevout.unwrap().value).sum();
        let total_out: u64 = tx.vout.iter().map(|vout| vout.value).sum();
        tx.fee = Some(total_in - total_out);
    }
}

pub fn run_server(config: &Config, query: Arc<Query>) {
    let addr = ([127, 0, 0, 1], 3000).into();  // TODO take from config
    info!("REST server running on {}", addr);

    let network = config.network_type;

    let new_service = move || {

        let query = query.clone();

        service_fn_ok(move |req: Request<Body>| {
            match handle_request(req,&query,&network) {
                Ok(response) => response,
                Err(e) => {
                    warn!("{:?}",e);
                    http_message(StatusCode::BAD_REQUEST, e.0)
                },
            }
        })
    };

    let server = Server::bind(&addr)
        .serve(new_service)
        .map_err(|e| eprintln!("server error: {}", e));

    thread::spawn(move || {
        rt::run(server);
    });
}

fn handle_request(req: Request<Body>, query: &Arc<Query>, network: &Network) -> Result<Response<Body>, StringError> {
    // TODO it looks hyper does not have routing and query parsing :(
    let uri = req.uri();
    let path: Vec<&str> = uri.path().split('/').skip(1).collect();
    info!("path {:?}", path);
    match (req.method(), path.get(0), path.get(1), path.get(2), path.get(3)) {
        (&Method::GET, Some(&"tip"), Some(&"hash"), None, None) =>
            Ok(http_message(StatusCode::OK, query.get_best_header_hash().be_hex_string())),

        (&Method::GET, Some(&"tip"), Some(&"height"), None, None) =>
            Ok(http_message(StatusCode::OK, query.get_best_height().to_string())),

        (&Method::GET, Some(&"blocks"), start_height, None, None) => {
            let start_height = start_height.and_then(|height| height.parse::<usize>().ok());
            blocks(&query, start_height)
        },
        (&Method::GET, Some(&"block-height"), Some(height), None, None) => {
            let height = height.parse::<usize>()?;
            match query.get_headers(&[height]).get(0) {
                None => Ok(http_message(StatusCode::NOT_FOUND, format!("can't find header at height {}", height))),
                Some(val) => Ok(http_message(StatusCode::OK, val.hash().be_hex_string()))
            }
        },
        (&Method::GET, Some(&"block"), Some(hash), None, None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let blockhm = query.get_block_header_with_meta(&hash)?;
            let block_value = BlockValue::from(blockhm);
            json_response(block_value)
        },
        (&Method::GET, Some(&"block"), Some(hash), Some(&"status"), None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let status = query.get_block_status(&hash);
            json_response(status)
        },
        (&Method::GET, Some(&"block"), Some(hash), Some(&"txs"), start_index) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let block = query.get_block(&hash)?;

            // @TODO optimization: skip deserializing transactions outside of range
            let start_index = start_index
                .map_or(0u32, |el| el.parse().unwrap_or(0))
                .max(0u32) as usize;

            if start_index >= block.txdata.len() {
                return Ok(http_message(StatusCode::NOT_FOUND, "start index out of range".to_string()));
            } else if start_index % TX_LIMIT != 0 {
                return Ok(http_message(StatusCode::BAD_REQUEST, format!("start index must be a multipication of {}", TX_LIMIT)));
            }

            let mut txs = block.txdata.iter().skip(start_index).take(TX_LIMIT).map(|tx| TransactionValue::from(tx.clone())).collect();
            attach_txs_data(&mut txs, network, query);
            json_response(txs)
        },
        (&Method::GET, Some(&"address"), Some(address), None, None) => {
            let script_hash = address_to_scripthash(address)?;
            let status = query.status(&script_hash[..])?;
            // @TODO create new AddressStatsValue struct?
            json_response(json!({ "address": address, "tx_count": status.history().len(),
                                  "confirmed_balance": status.confirmed_balance(), "mempool_balance": status.mempool_balance(),
                                  "total_received": status.total_received(), }))
        },
        (&Method::GET, Some(&"address"), Some(address), Some(&"txs"), start_index) => {
            let start_index = start_index
                .map_or(0u32, |el| el.parse().unwrap_or(0))
                .max(0u32) as usize;

            let script_hash = address_to_scripthash(address)?;
            let status = query.status(&script_hash[..])?;
            let txs = status.history_txs();

            if txs.len() == 0 {
                return json_response(json!([]));
            } else if start_index >= txs.len() {
                return Ok(http_message(StatusCode::NOT_FOUND, "start index out of range".to_string()));
            } else if start_index % TX_LIMIT != 0 {
                return Ok(http_message(StatusCode::BAD_REQUEST, format!("start index must be a multipication of {}", TX_LIMIT)));
            }

            let mut txs = txs.iter().skip(start_index).take(TX_LIMIT).map(|t| TransactionValue::from((*t).clone())).collect();
            attach_txs_data(&mut txs, network, query);

            json_response(txs)
        },
        (&Method::GET, Some(&"tx"), Some(hash), None, None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let transaction = query.tx_get(&hash).ok_or(StringError("cannot find tx".to_string()))?;
            let mut value = TransactionValue::from(transaction);
            let value = attach_tx_data(value, network, query);
            json_response(value)
        },
        (&Method::GET, Some(&"tx"), Some(hash), Some(&"hex"), None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let rawtx = query.tx_get_raw(&hash).ok_or(StringError("cannot find tx".to_string()))?;
            Ok(http_message(StatusCode::OK, hex::encode(rawtx)))
        },
        (&Method::GET, Some(&"tx"), Some(hash), Some(&"status"), None) => {
            let hash = Sha256dHash::from_hex(hash)?;
            let status = query.get_tx_status(&hash)?;
            json_response(status)
        },
        _ => {
            Err(StringError(format!("endpoint does not exist {:?}", uri.path())))
        }
    }
}

fn http_message(status: StatusCode, message: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("Content-Type", "text/plain")
        .header("Access-Control-Allow-Origin", "*")
        .body(Body::from(message))
        .unwrap()
}

fn json_response<T: Serialize>(value : T) -> Result<Response<Body>,StringError> {
    let value = serde_json::to_string(&value)?;
    Ok(Response::builder()
        .header("Content-type","application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(Body::from(value)).unwrap())
}

fn blocks(query: &Arc<Query>, start_height: Option<usize>)
    -> Result<Response<Body>,StringError> {

    let mut values = Vec::new();
    let mut current_hash = match start_height {
        Some(height) => query.get_headers(&[height]).get(0).ok_or(StringError("cannot find block".to_string()))?.hash().clone(),
        None => query.get_best_header()?.hash().clone(),
    };

    let zero = [0u8;32];
    for _ in 0..BLOCK_LIMIT {
        let blockhm = query.get_block_header_with_meta(&current_hash)?;
        current_hash = blockhm.header_entry.header().prev_blockhash.clone();
        values.push(BlockValue::from(blockhm));

        if &current_hash[..] == &zero[..] {
            break;
        }
    }
    json_response(values)
}

fn address_to_scripthash(addr: &str) -> Result<FullHash, StringError> {
    let addr = Address::from_str(addr)?;
    Ok(compute_script_hash(&addr.script_pubkey().into_bytes()))
}

#[derive(Debug)]
struct StringError(String);

// TODO boilerplate conversion, use macros or other better error handling
impl From<ParseIntError> for StringError {
    fn from(e: ParseIntError) -> Self {
        StringError(e.description().to_string())
    }
}
impl From<HexError> for StringError {
    fn from(e: HexError) -> Self {
        StringError(e.description().to_string())
    }
}
impl From<FromHexError> for StringError {
    fn from(e: FromHexError) -> Self {
        StringError(e.description().to_string())
    }
}
impl From<errors::Error> for StringError {
    fn from(e: errors::Error) -> Self {
        StringError(e.description().to_string())
    }
}
impl From<serde_json::Error> for StringError {
    fn from(e: serde_json::Error) -> Self {
        StringError(e.description().to_string())
    }
}
impl From<network::serialize::Error> for StringError {
    fn from(e: network::serialize::Error) -> Self {
        StringError(e.description().to_string())
    }
}


#[cfg(test)]
mod tests {
    use serde_json::Value;
    use rest::StringError;
    use std::collections::HashMap;

    #[test]
    fn test_parse_query_param() {
        let mut query_params = HashMap::new();

        query_params.insert("limit","10");
        let limit = query_params.get("limit")
            .map_or(10u32,|el| el.parse().unwrap_or(10u32) )
            .min(30u32);
        assert_eq!(10, limit);

        query_params.insert("limit","100");
        let limit = query_params.get("limit")
            .map_or(10u32,|el| el.parse().unwrap_or(10u32) )
            .min(30u32);
        assert_eq!(30, limit);

        query_params.insert("limit","5");
        let limit = query_params.get("limit")
            .map_or(10u32,|el| el.parse().unwrap_or(10u32) )
            .min(30u32);
        assert_eq!(5, limit);

        query_params.insert("limit","aaa");
        let limit = query_params.get("limit")
            .map_or(10u32,|el| el.parse().unwrap_or(10u32) )
            .min(30u32);
        assert_eq!(10, limit);

        query_params.remove("limit");
        let limit = query_params.get("limit")
            .map_or(10u32,|el| el.parse().unwrap_or(10u32) )
            .min(30u32);
        assert_eq!(10, limit);
    }

    #[test]
    fn test_parse_value_param() {
        let v : Value = json!({ "confirmations": 10 });

        let confirmations = v.get("confirmations").and_then(|el| el.as_u64())
            .ok_or(StringError("confirmations absent or not a u64".to_string()))
            .unwrap();

        assert_eq!(10, confirmations);

        let err = v
            .get("notexist").and_then(|el| el.as_u64())
            .ok_or(StringError("notexist absent or not a u64".to_string()));

        assert!(err.is_err());
    }
}


