use anyhow::{bail, Context, Result};
use bitcoin::{
    consensus::{deserialize, serialize},
    hashes::hex::{FromHex, ToHex},
    BlockHash, Txid,
};
use rayon::prelude::*;
use serde_derive::Deserialize;
use serde_json::{self, json, Value};

use std::collections::HashMap;
use std::iter::FromIterator;

use crate::{
    cache::Cache,
    config::Config,
    daemon::{self, extract_bitcoind_error, Daemon},
    merkle::Proof,
    metrics::Histogram,
    status::Status,
    tracker::Tracker,
    types::ScriptHash,
};

const ELECTRS_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: &str = "1.4";

const UNKNOWN_FEE: isize = -1; // (allowed by Electrum protocol)

/// Per-client Electrum protocol state
#[derive(Default)]
pub struct Client {
    tip: Option<BlockHash>,
    status: HashMap<ScriptHash, Status>,
}

#[derive(Deserialize)]
struct Request {
    id: Value,
    method: String,

    #[serde(default)]
    params: Value,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Requests {
    Single(Request),
    Batch(Vec<Request>),
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
#[serde(untagged)]
enum Version {
    Single(String),
    Range(String, String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TxGetArgs {
    Txid((Txid,)),
    TxidVerbose(Txid, bool),
}

impl From<TxGetArgs> for (Txid, bool) {
    fn from(args: TxGetArgs) -> Self {
        match args {
            TxGetArgs::Txid((txid,)) => (txid, false),
            TxGetArgs::TxidVerbose(txid, verbose) => (txid, verbose),
        }
    }
}

enum StandardError {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
}

enum RpcError {
    // JSON-RPC spec errors
    Standard(StandardError),
    // Electrum-specific errors
    BadRequest(anyhow::Error),
    DaemonError(daemon::RpcError),
}

impl RpcError {
    fn to_value(&self) -> Value {
        match self {
            RpcError::Standard(err) => match err {
                StandardError::ParseError => json!({"code": -32700, "message": "parse error"}),
                StandardError::InvalidRequest => {
                    json!({"code": -32600, "message": "invalid request"})
                }
                StandardError::MethodNotFound => {
                    json!({"code": -32601, "message": "method not found"})
                }
                StandardError::InvalidParams => {
                    json!({"code": -32602, "message": "invalid params"})
                }
            },
            RpcError::BadRequest(err) => json!({"code": 1, "message": err.to_string()}),
            RpcError::DaemonError(err) => json!({"code": 2, "message": err.message}),
        }
    }
}

/// Electrum RPC handler
pub struct Rpc {
    tracker: Tracker,
    cache: Cache,
    rpc_duration: Histogram,
    daemon: Daemon,
    banner: String,
}

impl Rpc {
    pub fn new(config: &Config, tracker: Tracker) -> Result<Self> {
        let rpc_duration =
            tracker
                .metrics()
                .histogram_vec("rpc_duration", "RPC duration (in seconds)", "method");
        Ok(Self {
            tracker,
            cache: Cache::default(),
            rpc_duration,
            daemon: Daemon::connect(&config)?,
            banner: config.server_banner.clone(),
        })
    }

    pub fn sync(&mut self) -> Result<()> {
        self.tracker.sync(&self.daemon)
    }

    pub fn update_client(&self, client: &mut Client) -> Result<Vec<String>> {
        let chain = self.tracker.chain();
        let mut notifications = client
            .status
            .par_iter_mut()
            .filter_map(|(scripthash, status)| -> Option<Result<Value>> {
                match self
                    .tracker
                    .update_status(status, &self.daemon, &self.cache)
                {
                    Ok(true) => Some(Ok(notification(
                        "blockchain.scripthash.subscribe",
                        &[json!(scripthash), json!(status.statushash())],
                    ))),
                    Ok(false) => None, // statushash is the same
                    Err(e) => Some(Err(e)),
                }
            })
            .collect::<Result<Vec<Value>>>()
            .context("failed to update status")?;

        if let Some(old_tip) = client.tip {
            let new_tip = self.tracker.chain().tip();
            if old_tip != new_tip {
                client.tip = Some(new_tip);
                let height = chain.height();
                let header = chain.get_block_header(height).unwrap();
                notifications.push(notification(
                    "blockchain.headers.subscribe",
                    &[json!({"hex": serialize(&header).to_hex(), "height": height})],
                ));
            }
        }
        Ok(notifications.into_iter().map(|v| v.to_string()).collect())
    }

    fn headers_subscribe(&self, client: &mut Client) -> Result<Value> {
        let chain = self.tracker.chain();
        client.tip = Some(chain.tip());
        let height = chain.height();
        let header = chain.get_block_header(height).unwrap();
        Ok(json!({"hex": serialize(header).to_hex(), "height": height}))
    }

    fn block_header(&self, (height,): (usize,)) -> Result<Value> {
        let chain = self.tracker.chain();
        let header = match chain.get_block_header(height) {
            None => bail!("no header at {}", height),
            Some(header) => header,
        };
        Ok(json!(serialize(header).to_hex()))
    }

    fn block_headers(&self, (start_height, count): (usize, usize)) -> Result<Value> {
        let chain = self.tracker.chain();
        let max_count = 2016usize;

        let count = std::cmp::min(
            std::cmp::min(count, max_count),
            chain.height() - start_height + 1,
        );
        let heights = start_height..(start_height + count);
        let hex_headers = String::from_iter(
            heights.map(|height| serialize(chain.get_block_header(height).unwrap()).to_hex()),
        );

        Ok(json!({"count": count, "hex": hex_headers, "max": max_count}))
    }

    fn estimate_fee(&self, (nblocks,): (u16,)) -> Result<Value> {
        Ok(self
            .daemon
            .estimate_fee(nblocks)?
            .map(|fee_rate| json!(fee_rate.as_btc()))
            .unwrap_or_else(|| json!(UNKNOWN_FEE)))
    }

    fn relayfee(&self) -> Result<Value> {
        Ok(json!(self.daemon.get_relay_fee()?.as_btc())) // [BTC/kB]
    }

    fn scripthash_get_balance(
        &self,
        client: &Client,
        (scripthash,): (ScriptHash,),
    ) -> Result<Value> {
        let balance = match client.status.get(&scripthash) {
            Some(status) => self.tracker.get_balance(status, &self.cache),
            None => {
                warn!(
                    "blockchain.scripthash.get_balance called for unsubscribed scripthash: {}",
                    scripthash
                );
                self.tracker
                    .get_balance(&self.new_status(scripthash)?, &self.cache)
            }
        };
        Ok(
            json!({"confirmed": balance.confirmed.as_sat(), "unconfirmed": balance.mempool_delta.as_sat()}),
        )
    }

    fn scripthash_get_history(
        &self,
        client: &Client,
        (scripthash,): (ScriptHash,),
    ) -> Result<Value> {
        let history_entries = match client.status.get(&scripthash) {
            Some(status) => self.tracker.get_history(status),
            None => {
                warn!(
                    "blockchain.scripthash.get_history called for unsubscribed scripthash: {}",
                    scripthash
                );
                self.tracker.get_history(&self.new_status(scripthash)?)
            }
        };
        Ok(json!(history_entries.collect::<Vec<Value>>()))
    }

    fn scripthash_subscribe(
        &self,
        client: &mut Client,
        (scripthash,): (ScriptHash,),
    ) -> Result<Value> {
        let status = self.new_status(scripthash)?;
        let statushash = status.statushash();
        client.status.insert(scripthash, status); // skip if already exists
        Ok(json!(statushash))
    }

    fn new_status(&self, scripthash: ScriptHash) -> Result<Status> {
        let mut status = Status::new(scripthash);
        self.tracker
            .update_status(&mut status, &self.daemon, &self.cache)?;
        Ok(status)
    }

    fn transaction_broadcast(&self, (tx_hex,): (String,)) -> Result<Value> {
        let tx_bytes = Vec::from_hex(&tx_hex).context("non-hex transaction")?;
        let tx = deserialize(&tx_bytes).context("invalid transaction")?;
        let txid = self.daemon.broadcast(&tx)?;
        Ok(json!(txid))
    }

    fn transaction_get(&self, args: TxGetArgs) -> Result<Value> {
        let (txid, verbose) = args.into();
        if verbose {
            let blockhash = self.tracker.get_blockhash_by_txid(txid);
            return Ok(json!(self.daemon.get_transaction_info(&txid, blockhash)?));
        }
        let cached = self.cache.get_tx(&txid, |tx| serialize(tx).to_hex());
        Ok(match cached {
            Some(tx_hex) => json!(tx_hex),
            None => {
                debug!("tx cache miss: {}", txid);
                let blockhash = self.tracker.get_blockhash_by_txid(txid);
                json!(self.daemon.get_transaction_hex(&txid, blockhash)?)
            }
        })
    }

    fn transaction_get_merkle(&self, (txid, height): (Txid, usize)) -> Result<Value> {
        let chain = self.tracker.chain();
        let blockhash = match chain.get_block_hash(height) {
            None => bail!("missing block at {}", height),
            Some(blockhash) => blockhash,
        };
        let proof_to_value = |proof: &Proof| {
            json!({
                "block_height": height,
                "pos": proof.position(),
                "merkle": proof.to_hex(),
            })
        };
        if let Some(result) = self.cache.get_proof(blockhash, txid, proof_to_value) {
            return Ok(result);
        }
        debug!("txids cache miss: {}", blockhash);
        let txids = self.daemon.get_block_txids(blockhash)?;
        match txids.iter().position(|current_txid| *current_txid == txid) {
            None => bail!("missing txid {} in block {}", txid, blockhash),
            Some(position) => Ok(proof_to_value(&Proof::create(&txids, position))),
        }
    }

    fn get_fee_histogram(&self) -> Result<Value> {
        Ok(json!(self.tracker.fees_histogram()))
    }

    fn version(&self, (client_id, client_version): (String, Version)) -> Result<Value> {
        match client_version {
            Version::Single(v) if v == PROTOCOL_VERSION => {
                let server_id = format!("electrs/{}", ELECTRS_VERSION);
                Ok(json!([server_id, PROTOCOL_VERSION]))
            }
            _ => {
                bail!(
                    "{} requested {:?}, server supports {}",
                    client_id,
                    client_version,
                    PROTOCOL_VERSION
                );
            }
        }
    }

    pub fn handle_request(&self, client: &mut Client, line: &str) -> String {
        let error_msg_no_id = |err| error_msg(Value::Null, RpcError::Standard(err));
        let response: Value = match serde_json::from_str(line) {
            // parse JSON from str
            Ok(value) => match serde_json::from_value(value) {
                // parse RPC from JSON
                Ok(requests) => match requests {
                    Requests::Single(request) => self.call(client, request),
                    Requests::Batch(requests) => json!(requests
                        .into_iter()
                        .map(|request| self.call(client, request))
                        .collect::<Vec<Value>>()),
                },
                Err(err) => {
                    warn!("invalid RPC request ({:?}): {}", line, err);
                    error_msg_no_id(StandardError::InvalidRequest)
                }
            },
            Err(err) => {
                warn!("invalid JSON ({:?}): {}", line, err);
                error_msg_no_id(StandardError::ParseError)
            }
        };
        response.to_string()
    }

    fn call(&self, client: &mut Client, request: Request) -> Value {
        let Request { id, method, params } = request;
        let call = match Call::parse(&method, params) {
            Ok(call) => call,
            Err(err) => return error_msg(id, RpcError::Standard(err)),
        };
        self.rpc_duration.observe_duration(&method, || {
            let result = match call {
                Call::Banner => Ok(json!(self.banner)),
                Call::BlockHeader(args) => self.block_header(args),
                Call::BlockHeaders(args) => self.block_headers(args),
                Call::Donation => Ok(Value::Null),
                Call::EstimateFee(args) => self.estimate_fee(args),
                Call::HeadersSubscribe => self.headers_subscribe(client),
                Call::MempoolFeeHistogram => self.get_fee_histogram(),
                Call::PeersSubscribe => Ok(json!([])),
                Call::Ping => Ok(Value::Null),
                Call::RelayFee => self.relayfee(),
                Call::ScriptHashGetBalance(args) => self.scripthash_get_balance(client, args),
                Call::ScriptHashGetHistory(args) => self.scripthash_get_history(client, args),
                Call::ScriptHashSubscribe(args) => self.scripthash_subscribe(client, args),
                Call::TransactionBroadcast(args) => self.transaction_broadcast(args),
                Call::TransactionGet(args) => self.transaction_get(args),
                Call::TransactionGetMerkle(args) => self.transaction_get_merkle(args),
                Call::Version(args) => self.version(args),
            };
            match result {
                Ok(value) => result_msg(id, value),
                Err(err) => {
                    warn!("RPC {} failed: {:#}", method, err);
                    match err
                        .downcast_ref::<bitcoincore_rpc::Error>()
                        .and_then(extract_bitcoind_error)
                    {
                        Some(e) => error_msg(id, RpcError::DaemonError(e.clone())),
                        None => error_msg(id, RpcError::BadRequest(err)),
                    }
                }
            }
        })
    }
}

#[derive(Deserialize)]
enum Call {
    Banner,
    BlockHeader((usize,)),
    BlockHeaders((usize, usize)),
    TransactionBroadcast((String,)),
    Donation,
    EstimateFee((u16,)),
    HeadersSubscribe,
    MempoolFeeHistogram,
    PeersSubscribe,
    Ping,
    RelayFee,
    ScriptHashGetBalance((ScriptHash,)),
    ScriptHashGetHistory((ScriptHash,)),
    ScriptHashSubscribe((ScriptHash,)),
    TransactionGet(TxGetArgs),
    TransactionGetMerkle((Txid, usize)),
    Version((String, Version)),
}

impl Call {
    fn parse(method: &str, params: Value) -> std::result::Result<Call, StandardError> {
        Ok(match method {
            "blockchain.block.header" => Call::BlockHeader(convert(params)?),
            "blockchain.block.headers" => Call::BlockHeaders(convert(params)?),
            "blockchain.estimatefee" => Call::EstimateFee(convert(params)?),
            "blockchain.headers.subscribe" => Call::HeadersSubscribe,
            "blockchain.relayfee" => Call::RelayFee,
            "blockchain.scripthash.get_balance" => Call::ScriptHashGetBalance(convert(params)?),
            "blockchain.scripthash.get_history" => Call::ScriptHashGetHistory(convert(params)?),
            "blockchain.scripthash.subscribe" => Call::ScriptHashSubscribe(convert(params)?),
            "blockchain.transaction.broadcast" => Call::TransactionBroadcast(convert(params)?),
            "blockchain.transaction.get" => Call::TransactionGet(convert(params)?),
            "blockchain.transaction.get_merkle" => Call::TransactionGetMerkle(convert(params)?),
            "mempool.get_fee_histogram" => Call::MempoolFeeHistogram,
            "server.banner" => Call::Banner,
            "server.donation_address" => Call::Donation,
            "server.peers.subscribe" => Call::PeersSubscribe,
            "server.ping" => Call::Ping,
            "server.version" => Call::Version(convert(params)?),
            _ => {
                warn!("unknown method {}", method);
                return Err(StandardError::MethodNotFound);
            }
        })
    }
}

fn convert<T>(params: Value) -> std::result::Result<T, StandardError>
where
    T: serde::de::DeserializeOwned,
{
    let params_str = params.to_string();
    serde_json::from_value(params).map_err(|err| {
        warn!("invalid params {}: {}", params_str, err);
        StandardError::InvalidParams
    })
}

fn notification(method: &str, params: &[Value]) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

fn result_msg(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_msg(id: Value, error: RpcError) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": error.to_value()})
}
