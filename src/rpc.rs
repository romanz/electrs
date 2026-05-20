//! Minimal Bitcoin Core JSON-RPC client.
//!
//! Replaces the archived `bitcoincore-rpc` crate (see issue #1254). Implements only
//! the subset of methods and response fields that electrs actually consumes, layered
//! directly on top of the still-maintained `jsonrpc` crate.

use std::fmt;
use std::path::PathBuf;

use bitcoin::consensus::encode::serialize_hex;
use bitcoin::{Amount, BlockHash, Transaction, Txid};
use serde_json::{json, value::to_raw_value, Value};

pub use jsonrpc;
pub use jsonrpc::error::RpcError;

/// Error type returned by RPC calls.
///
/// Mirrors the two-layer shape callers used to pattern-match against on the old
/// `bitcoincore_rpc::Error`: a JSON-RPC layer error or a local decode failure.
#[derive(Debug)]
pub enum Error {
    JsonRpc(jsonrpc::Error),
    /// Decoding hex- or consensus-encoded data returned by bitcoind failed.
    Decode(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::JsonRpc(e) => write!(f, "JSON-RPC error: {}", e),
            Error::Decode(msg) => write!(f, "failed to decode bitcoind response: {}", msg),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::JsonRpc(e) => Some(e),
            Error::Decode(_) => None,
        }
    }
}

impl From<jsonrpc::Error> for Error {
    fn from(e: jsonrpc::Error) -> Self {
        Error::JsonRpc(e)
    }
}

/// Result alias parallel to the old `bitcoincore_rpc::Result`.
pub type Result<T> = std::result::Result<T, Error>;

/// Authentication mode for connecting to bitcoind's RPC endpoint.
#[derive(Clone, Debug)]
pub enum Auth {
    /// No authentication. Kept for API completeness and matched by the connect path,
    /// even though config.rs currently always populates one of the other variants.
    #[allow(dead_code)]
    None,
    UserPass(String, String),
    CookieFile(PathBuf),
}

/// Thin typed wrapper over [`jsonrpc::Client`] exposing the methods electrs uses.
pub struct Client {
    inner: jsonrpc::Client,
}

impl Client {
    /// Wrap a pre-built `jsonrpc::Client` (so callers control the transport).
    pub fn from_jsonrpc(inner: jsonrpc::Client) -> Self {
        Self { inner }
    }

    /// Borrow the underlying `jsonrpc::Client` for batch requests.
    pub fn get_jsonrpc_client(&self) -> &jsonrpc::Client {
        &self.inner
    }

    /// Generic RPC call — serializes args to JSON and deserializes the response.
    pub fn call<T>(&self, method: &str, args: &[Value]) -> Result<T>
    where
        T: for<'a> serde::de::Deserialize<'a>,
    {
        // `jsonrpc::Client::call` expects the whole params array packed into a single
        // `RawValue`, not a slice of per-arg `RawValue`s.
        let params = if args.is_empty() {
            None
        } else {
            let array = Value::Array(args.to_vec());
            Some(to_raw_value(&array).map_err(|e| Error::JsonRpc(jsonrpc::Error::Json(e)))?)
        };
        Ok(self.inner.call(method, params.as_deref())?)
    }

    pub fn get_network_info(&self) -> Result<GetNetworkInfoResult> {
        self.call("getnetworkinfo", &[])
    }

    pub fn get_blockchain_info(&self) -> Result<GetBlockchainInfoResult> {
        self.call("getblockchaininfo", &[])
    }

    pub fn estimate_smart_fee(
        &self,
        conf_target: u16,
        estimate_mode: Option<&str>,
    ) -> Result<EstimateSmartFeeResult> {
        let mut args = vec![json!(conf_target)];
        if let Some(mode) = estimate_mode {
            args.push(json!(mode));
        }
        self.call("estimatesmartfee", &args)
    }

    pub fn send_raw_transaction(&self, tx: &Transaction) -> Result<Txid> {
        self.call("sendrawtransaction", &[json!(serialize_hex(tx))])
    }

    pub fn get_raw_transaction(
        &self,
        txid: &Txid,
        blockhash: Option<&BlockHash>,
    ) -> Result<Transaction> {
        let hex: String = self.call(
            "getrawtransaction",
            &[json!(txid), json!(false), json!(blockhash)],
        )?;
        let bytes = <Vec<u8> as bitcoin::hashes::hex::FromHex>::from_hex(&hex)
            .map_err(|e| Error::Decode(format!("hex decode failed: {}", e)))?;
        bitcoin::consensus::deserialize(&bytes)
            .map_err(|e| Error::Decode(format!("transaction decode failed: {}", e)))
    }

    pub fn get_block_info(&self, blockhash: &BlockHash) -> Result<GetBlockResult> {
        // Verbosity 1 = JSON with txids only (matches what electrs reads: .tx).
        self.call("getblock", &[json!(blockhash), json!(1)])
    }

    pub fn get_mempool_info(&self) -> Result<GetMempoolInfoResult> {
        self.call("getmempoolinfo", &[])
    }

    pub fn get_raw_mempool(&self) -> Result<Vec<Txid>> {
        self.call("getrawmempool", &[])
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetNetworkInfoResult {
    pub version: usize,
    #[serde(rename = "networkactive")]
    pub network_active: bool,
    #[serde(rename = "relayfee", with = "bitcoin::amount::serde::as_btc")]
    pub relay_fee: Amount,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetBlockchainInfoResult {
    pub blocks: u64,
    pub headers: u64,
    pub pruned: bool,
    #[serde(rename = "initialblockdownload")]
    pub initial_block_download: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EstimateSmartFeeResult {
    #[serde(
        default,
        rename = "feerate",
        with = "bitcoin::amount::serde::as_btc::opt"
    )]
    pub fee_rate: Option<Amount>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetBlockResult {
    pub tx: Vec<Txid>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetMempoolInfoResult {
    /// Bitcoin Core 0.21+ field; older nodes omit it. Caller treats `None` as loaded.
    pub loaded: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetMempoolEntryResult {
    pub vsize: u64,
    pub fees: GetMempoolEntryResultFees,
    pub depends: Vec<Txid>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetMempoolEntryResultFees {
    #[serde(with = "bitcoin::amount::serde::as_btc")]
    pub base: Amount,
}
