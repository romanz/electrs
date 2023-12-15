use anyhow::{Context, Result};

use bitcoin::{consensus::deserialize, hashes::hex::FromHex};
use bitcoin::{Amount, BlockHash, Transaction, Txid};
use bitcoincore_rpc::{json, jsonrpc, Auth, Client, RpcApi};
use crossbeam_channel::Receiver;
use parking_lot::Mutex;
use serde_json::{json, Value};

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::{
    chain::{Chain, NewHeader},
    config::Config,
    metrics::Metrics,
    p2p::Connection,
    signals::ExitFlag,
    types::SerBlock,
};

enum PollResult {
    Done(Result<()>),
    Retry,
}

fn rpc_poll(client: &mut Client, skip_block_download_wait: bool) -> PollResult {
    match client.get_blockchain_info() {
        Ok(info) => {
            if skip_block_download_wait {
                // bitcoind RPC is available, don't wait for block download to finish
                return PollResult::Done(Ok(()));
            }
            let left_blocks = info.headers - info.blocks;
            if info.initial_block_download || left_blocks > 0 {
                info!(
                    "waiting for {} blocks to download{}",
                    left_blocks,
                    if info.initial_block_download {
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

fn rpc_connect(config: &Config) -> Result<Client> {
    let rpc_url = format!("http://{}", config.daemon_rpc_addr);
    // Allow `wait_for_new_block` to take a bit longer before timing out.
    // See https://github.com/romanz/electrs/issues/495 for more details.
    let builder = jsonrpc::simple_http::SimpleHttpTransport::builder()
        .url(&rpc_url)?
        .timeout(config.jsonrpc_timeout);
    let builder = match config.daemon_auth.get_auth() {
        Auth::None => builder,
        Auth::UserPass(user, pass) => builder.auth(user, Some(pass)),
        Auth::CookieFile(path) => {
            let (user, pass) = read_cookie(&path)?;
            builder.auth(user, Some(pass))
        }
    };
    Ok(Client::from_jsonrpc(jsonrpc::Client::with_transport(
        builder.build(),
    )))
}

pub struct Daemon {
    p2p: Mutex<Connection>,
    rpc: Client,
}

impl Daemon {
    pub(crate) fn connect(
        config: &Config,
        exit_flag: &ExitFlag,
        metrics: &Metrics,
    ) -> Result<Self> {
        let mut rpc = rpc_connect(config)?;

        loop {
            exit_flag
                .poll()
                .context("bitcoin RPC polling interrupted")?;
            match rpc_poll(&mut rpc, config.skip_block_download_wait) {
                PollResult::Done(result) => {
                    result.context("bitcoind RPC polling failed")?;
                    break; // on success, finish polling
                }
                PollResult::Retry => {
                    std::thread::sleep(std::time::Duration::from_secs(1)); // wait a bit before polling
                }
            }
        }

        let network_info = rpc.get_network_info()?;
        if network_info.version < 21_00_00 {
            bail!("electrs requires bitcoind 0.21+");
        }
        if !network_info.network_active {
            bail!("electrs requires active bitcoind p2p network");
        }
        let info = rpc.get_blockchain_info()?;
        if info.pruned {
            bail!("electrs requires non-pruned bitcoind node");
        }

        let p2p = Mutex::new(Connection::connect(
            config.network,
            config.daemon_p2p_addr,
            metrics,
            config.signet_magic,
        )?);
        Ok(Self { p2p, rpc })
    }

    pub(crate) fn estimate_fee(&self, nblocks: u16) -> Result<Option<Amount>> {
        Ok(self
            .rpc
            .estimate_smart_fee(nblocks, None)
            .context("failed to estimate fee")?
            .fee_rate)
    }

    pub(crate) fn get_relay_fee(&self) -> Result<Amount> {
        Ok(self
            .rpc
            .get_network_info()
            .context("failed to get relay fee")?
            .relay_fee)
    }

    pub(crate) fn broadcast(&self, tx: &Transaction) -> Result<Txid> {
        self.rpc
            .send_raw_transaction(tx)
            .context("failed to broadcast transaction")
    }

    pub(crate) fn get_transaction_info(
        &self,
        txid: &Txid,
        blockhash: Option<BlockHash>,
    ) -> Result<Value> {
        // No need to parse the resulting JSON, just return it as-is to the client.
        self.rpc
            .call(
                "getrawtransaction",
                &[json!(txid), json!(true), json!(blockhash)],
            )
            .context("failed to get transaction info")
    }

    pub(crate) fn get_transaction_hex(
        &self,
        txid: &Txid,
        blockhash: Option<BlockHash>,
    ) -> Result<Value> {
        use bitcoin::consensus::serde::{hex::Lower, Hex, With};

        let tx = self.get_transaction(txid, blockhash)?;
        #[derive(serde::Serialize)]
        #[serde(transparent)]
        struct TxAsHex(#[serde(with = "With::<Hex<Lower>>")] Transaction);
        serde_json::to_value(TxAsHex(tx)).map_err(Into::into)
    }

    pub(crate) fn get_transaction(
        &self,
        txid: &Txid,
        blockhash: Option<BlockHash>,
    ) -> Result<Transaction> {
        self.rpc
            .get_raw_transaction(txid, blockhash.as_ref())
            .context("failed to get transaction")
    }

    pub(crate) fn get_block_txids(&self, blockhash: BlockHash) -> Result<Vec<Txid>> {
        Ok(self
            .rpc
            .get_block_info(&blockhash)
            .context("failed to get block txids")?
            .tx)
    }

    pub(crate) fn get_mempool_txids(&self) -> Result<Vec<Txid>> {
        self.rpc
            .get_raw_mempool()
            .context("failed to get mempool txids")
    }

    pub(crate) fn get_mempool_entries(
        &self,
        txids: &[Txid],
    ) -> Result<Vec<Result<json::GetMempoolEntryResult>>> {
        let client = self.rpc.get_jsonrpc_client();
        debug!("getting {} mempool entries", txids.len());
        let args: Vec<_> = txids
            .iter()
            .map(|txid| vec![serde_json::value::to_raw_value(txid).unwrap()])
            .collect();
        let reqs: Vec<_> = args
            .iter()
            .map(|a| client.build_request("getmempoolentry", a))
            .collect();
        let res = client.send_batch(&reqs).context("batch request failed")?;
        debug!("got {} mempool entries", res.len());
        Ok(res
            .into_iter()
            .map(|r| {
                r.context("missing response")?
                    .result::<json::GetMempoolEntryResult>()
                    .context("invalid response")
            })
            .collect())
    }

    pub(crate) fn get_mempool_transactions(
        &self,
        txids: &[Txid],
    ) -> Result<Vec<Result<Transaction>>> {
        let client = self.rpc.get_jsonrpc_client();
        debug!("getting {} transactions", txids.len());
        let args: Vec<_> = txids
            .iter()
            .map(|txid| vec![serde_json::value::to_raw_value(txid).unwrap()])
            .collect();
        let reqs: Vec<_> = args
            .iter()
            .map(|a| client.build_request("getrawtransaction", a))
            .collect();
        let res = client.send_batch(&reqs).context("batch request failed")?;
        debug!("got {} mempool transactions", res.len());
        Ok(res
            .into_iter()
            .map(|r| -> Result<Transaction> {
                let tx_hex = r
                    .context("missing response")?
                    .result::<String>()
                    .context("invalid response")?;
                let tx_bytes = Vec::from_hex(&tx_hex).context("non-hex transaction")?;
                deserialize(&tx_bytes).context("invalid transaction")
            })
            .collect())
    }

    pub(crate) fn get_new_headers(&self, chain: &Chain) -> Result<Vec<NewHeader>> {
        self.p2p.lock().get_new_headers(chain)
    }

    pub(crate) fn for_blocks<B, F>(&self, blockhashes: B, func: F) -> Result<()>
    where
        B: IntoIterator<Item = BlockHash>,
        F: FnMut(BlockHash, SerBlock),
    {
        self.p2p.lock().for_blocks(blockhashes, func)
    }

    pub(crate) fn new_block_notification(&self) -> Receiver<()> {
        self.p2p.lock().new_block_notification()
    }
}

pub(crate) type RpcError = bitcoincore_rpc::jsonrpc::error::RpcError;

pub(crate) fn extract_bitcoind_error(err: &bitcoincore_rpc::Error) -> Option<&RpcError> {
    use bitcoincore_rpc::{
        jsonrpc::error::Error::Rpc as ServerError, Error::JsonRpc as JsonRpcError,
    };
    match err {
        JsonRpcError(ServerError(e)) => Some(e),
        _ => None,
    }
}
