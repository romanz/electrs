use anyhow::{Context, Result};

use crate::bitcoin::{
    self, consensus::deserialize, consensus::encode::serialize_hex, hashes::hex::FromHex, Amount,
    BlockHash, Transaction, Txid,
};
use crate::types::Auth;
use serde::Serialize;
use serde_json::{json, value::RawValue, Value};

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::{config::Config, signals::ExitFlag};

enum PollResult {
    Done(Result<()>),
    Retry,
}

#[derive(Deserialize)]
pub struct GetMempoolEntryResultFees {
    /// Transaction fee in BTC
    #[serde(with = "bitcoin::amount::serde::as_btc")]
    pub base: Amount,
}

#[derive(Deserialize)]
pub struct GetMempoolEntryResult {
    /// Virtual transaction size as defined in BIP 141. This is different from actual serialized
    /// size for witness transactions as witness data is discounted.
    #[serde(alias = "size")]
    pub vsize: u64,
    /// Fee information
    pub fees: GetMempoolEntryResultFees,
    /// Unconfirmed transactions used as inputs for this transaction
    pub depends: Vec<bitcoin::Txid>,
}

#[derive(Deserialize)]
pub struct GetBlockchainInfoResult {
    pub blocks: u64,
    pub headers: u64,
    pub initialblockdownload: bool,
    pub pruned: bool,
}

#[derive(Deserialize)]
pub struct EstimateSmartFeeResult {
    /// Estimate fee rate in BTC/kB.
    #[serde(
        default,
        rename = "feerate",
        skip_serializing_if = "Option::is_none",
        with = "bitcoin::amount::serde::as_btc::opt"
    )]
    pub fee_rate: Option<Amount>,
}

#[derive(Deserialize)]
pub struct GetNetworkInfoResult {
    #[serde(rename = "relayfee", with = "bitcoin::amount::serde::as_btc")]
    pub relay_fee: Amount,
}

#[derive(Deserialize)]
pub struct GetMempoolInfoResult {
    /// True if the mempool is fully loaded
    pub loaded: Option<bool>,
}

fn read_cookie(path: &Path) -> Result<(String, String)> {
    // Load username and password from bitcoind cookie file:
    // * https://github.com/bitcoin/bitcoin/pull/6388/commits/71cbeaad9a929ba6a7b62d9b37a09b214ae00c1a
    // * https://bitcoin.stackexchange.com/questions/46782/rpc-cookie-authentication
    let mut file = File::open(path)
        .with_context(|| format!("failed to open bitcoind cookie file: {}", path.display()))?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .with_context(|| format!("failed to read bitcoind cookie from {}", path.display()))?;

    let parts: Vec<&str> = contents.splitn(2, ':').collect();
    ensure!(
        parts.len() == 2,
        "failed to parse bitcoind cookie - missing ':' separator"
    );
    Ok((parts[0].to_owned(), parts[1].to_owned()))
}

fn jsonrpc_client(config: &Config) -> Result<jsonrpc::Client> {
    let rpc_url = format!("http://{}", config.daemon_rpc_addr);
    // Allow RPC calls to take longer before timing out.
    // See https://github.com/romanz/electrs/issues/495 for more details.
    let builder = jsonrpc::simple_http::SimpleHttpTransport::builder()
        .url(&rpc_url)?
        .timeout(config.jsonrpc_timeout);
    let builder = match config.daemon_auth.get_auth() {
        Auth::UserPass(user, pass) => builder.auth(user, Some(pass)),
        Auth::CookieFile(path) => {
            let (user, pass) = read_cookie(&path)?;
            builder.auth(user, Some(pass))
        }
    };
    Ok(jsonrpc::Client::with_transport(builder.build()))
}

pub struct Daemon {
    client: jsonrpc::Client,
}

impl Daemon {
    pub(crate) fn connect(config: &Config, exit_flag: &ExitFlag) -> Result<Self> {
        let daemon = Daemon {
            client: jsonrpc_client(config)?,
        };

        loop {
            exit_flag
                .poll()
                .context("bitcoin RPC polling interrupted")?;
            match daemon.rpc_poll(config.skip_block_download_wait) {
                PollResult::Done(result) => {
                    result.context("bitcoind RPC polling failed")?;
                    break; // on success, finish polling
                }
                PollResult::Retry => {
                    std::thread::sleep(std::time::Duration::from_secs(1)); // wait a bit before polling
                }
            }
        }

        Ok(daemon)
    }

    fn call<T: for<'a> serde::de::Deserialize<'a>>(
        &self,
        cmd: &str,
        args: &[serde_json::Value],
    ) -> Result<T, jsonrpc::Error> {
        let raw = serde_json::value::to_raw_value(args)?;
        let req = self.client.build_request(cmd, Some(&*raw));
        let resp = self.client.send_request(req)?;
        resp.result()
    }

    fn batch_request<T>(&self, name: &str, items: &[T]) -> Result<Vec<Option<jsonrpc::Response>>>
    where
        T: Serialize,
    {
        debug!("calling {} on {} items", name, items.len());
        let args: Vec<Box<RawValue>> = items
            .iter()
            .map(|item| jsonrpc::try_arg([item]).context("failed to serialize into JSON"))
            .collect::<Result<Vec<_>>>()?;
        let reqs: Vec<jsonrpc::Request> = args
            .iter()
            .map(|arg| self.client.build_request(name, Some(arg)))
            .collect();
        match self.client.send_batch(&reqs) {
            Ok(values) => {
                assert_eq!(items.len(), values.len());
                Ok(values)
            }
            Err(err) => bail!("batch {} request failed: {}", name, err),
        }
    }

    fn rpc_poll(&self, skip_block_download_wait: bool) -> PollResult {
        match self.call::<GetBlockchainInfoResult>("getblockchaininfo", &[]) {
            Ok(info) => {
                if info.pruned {
                    return PollResult::Done(Err(anyhow!(
                        "electrs requires non-pruned bitcoind node"
                    )));
                }
                if skip_block_download_wait {
                    // bitcoind RPC is available, don't wait for block download to finish
                    return PollResult::Done(Ok(()));
                }
                let left_blocks = info.headers - info.blocks;
                if info.initialblockdownload || left_blocks > 0 {
                    info!(
                        "waiting for {} blocks to download{}",
                        left_blocks,
                        if info.initialblockdownload {
                            " (IBD)"
                        } else {
                            ""
                        }
                    );
                    return PollResult::Retry;
                }
                PollResult::Done(Ok(()))
            }
            Err(err) => {
                if let Some(e) = extract_bitcoind_error(&err) {
                    if e.code == -28 {
                        debug!("waiting for RPC warmup: {}", e.message);
                        return PollResult::Retry;
                    }
                }
                PollResult::Done(Err(err).context("daemon not available"))
            }
        }
    }

    pub(crate) fn estimate_fee(&self, nblocks: u16) -> Result<Option<Amount>> {
        let res =
            self.call::<EstimateSmartFeeResult>("estimatesmartfee", &[json!(nblocks), Value::Null]);
        if let Err(jsonrpc::Error::Rpc(jsonrpc::error::RpcError { code: -32603, .. })) = res {
            return Ok(None); // don't fail when fee estimation is disabled (e.g. with `-blocksonly=1`)
        }
        Ok(res.context("failed to estimate fee")?.fee_rate)
    }

    pub(crate) fn get_relay_fee(&self) -> Result<Amount> {
        Ok(self
            .call::<GetNetworkInfoResult>("getnetworkinfo", &[])
            .context("failed to get relay fee")?
            .relay_fee)
    }

    pub(crate) fn broadcast(&self, tx: &Transaction) -> Result<Txid> {
        self.call("sendrawtransaction", &[json!(serialize_hex(tx))])
            .context("failed to broadcast transaction")
    }

    pub(crate) fn submitpackage(&self, txs: &[Transaction]) -> Result<Value> {
        let package: Vec<String> = txs.iter().map(serialize_hex).collect();
        self.call("submitpackage", &[json!(package)])
            .context("failed to submitpackage package")
    }

    pub(crate) fn get_transaction(
        &self,
        txid: &Txid,
        blockhash: Option<BlockHash>,
        verbose: bool,
    ) -> Result<Value> {
        self.call(
            "getrawtransaction",
            &[json!(txid), json!(verbose), json!(blockhash)],
        )
        .context("failed to get transaction")
    }

    pub(crate) fn get_block_txids(&self, blockhash: BlockHash) -> Result<Vec<Txid>> {
        #[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
        struct GetBlockResult {
            pub tx: Vec<bitcoin::Txid>,
        }
        let res: GetBlockResult = self
            .call("getblock", &[json!(blockhash), json!(1)])
            .context("failed to get block txids")?;
        Ok(res.tx)
    }

    pub(crate) fn get_mempool_info(&self) -> Result<GetMempoolInfoResult> {
        self.call("getmempoolinfo", &[])
            .context("failed to get mempool info")
    }

    pub(crate) fn get_mempool_txids(&self) -> Result<Vec<Txid>> {
        self.call("getrawmempool", &[])
            .context("failed to get mempool txids")
    }

    pub(crate) fn get_mempool_entries(
        &self,
        txids: &[Txid],
    ) -> Result<Vec<Option<GetMempoolEntryResult>>> {
        let results = self.batch_request("getmempoolentry", txids)?;
        Ok(results
            .into_iter()
            .map(|r| match r?.result::<GetMempoolEntryResult>() {
                Ok(entry) => Some(entry),
                Err(err) => {
                    debug!("failed to get mempool entry: {}", err); // probably due to RBF
                    None
                }
            })
            .collect())
    }

    pub(crate) fn get_mempool_transactions(
        &self,
        txids: &[Txid],
    ) -> Result<Vec<Option<Transaction>>> {
        let results = self.batch_request("getrawtransaction", txids)?;
        Ok(results
            .into_iter()
            .map(|r| -> Option<Transaction> {
                let tx_hex = match r?.result::<String>() {
                    Ok(tx_hex) => Some(tx_hex),
                    Err(err) => {
                        debug!("failed to get mempool tx: {}", err); // probably due to RBF
                        None
                    }
                }?;
                let tx_bytes = match Vec::from_hex(&tx_hex) {
                    Ok(tx_bytes) => Some(tx_bytes),
                    Err(err) => {
                        warn!("got non-hex transaction {}: {}", tx_hex, err);
                        None
                    }
                }?;
                match deserialize(&tx_bytes) {
                    Ok(tx) => Some(tx),
                    Err(err) => {
                        warn!("got invalid tx {}: {}", tx_hex, err);
                        None
                    }
                }
            })
            .collect())
    }
}

pub(crate) fn extract_bitcoind_error(err: &jsonrpc::Error) -> Option<&jsonrpc::error::RpcError> {
    match err {
        jsonrpc::error::Error::Rpc(e) => Some(e),
        _ => None,
    }
}
