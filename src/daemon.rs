use anyhow::{Context, Result};

use bitcoin::{Amount, Block, BlockHash, Transaction, Txid};
use bitcoincore_rpc::{self, json, RpcApi};
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::{
    chain::{Chain, NewHeader},
    config::Config,
    p2p::Connection,
};

enum PollResult {
    Done(Result<()>),
    Retry,
}

fn rpc_poll(client: &mut bitcoincore_rpc::Client) -> PollResult {
    match client.get_blockchain_info() {
        Ok(info) => {
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
                    info!("waiting for RPC warmup: {}", e.message);
                    return PollResult::Retry;
                }
            }
            PollResult::Done(Err(err).context("daemon not available"))
        }
    }
}

pub(crate) fn rpc_connect(config: &Config) -> Result<bitcoincore_rpc::Client> {
    let rpc_url = format!("http://{}", config.daemon_rpc_addr);
    if !config.daemon_cookie_file.exists() {
        bail!("{:?} is missing", config.daemon_cookie_file);
    }
    let rpc_auth = bitcoincore_rpc::Auth::CookieFile(config.daemon_cookie_file.clone());
    let mut client = bitcoincore_rpc::Client::new(rpc_url, rpc_auth)
        .with_context(|| format!("failed to connect to RPC: {}", config.daemon_rpc_addr))?;

    loop {
        match rpc_poll(&mut client) {
            PollResult::Done(result) => return result.map(|()| client),
            PollResult::Retry => {
                std::thread::sleep(std::time::Duration::from_secs(1)); // wait a bit before polling
            }
        }
    }
}

pub struct Daemon {
    p2p: Mutex<Connection>,
    rpc: bitcoincore_rpc::Client,
}

impl Daemon {
    pub fn connect(config: &Config) -> Result<Self> {
        let rpc = rpc_connect(config)?;
        let network_info = rpc.get_network_info()?;
        if network_info.version < 21_00_00 {
            bail!("electrs requires bitcoind 0.21+");
        }
        if !network_info.network_active {
            bail!("electrs requires active bitcoind p2p network");
        }
        let blockchain_info = rpc.get_blockchain_info()?;
        if blockchain_info.pruned {
            bail!("electrs requires non-pruned bitcoind node");
        }
        let p2p = Mutex::new(Connection::connect(config.network, config.daemon_p2p_addr)?);
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
    ) -> Result<json::GetRawTransactionResult> {
        self.rpc
            .get_raw_transaction_info(txid, blockhash.as_ref())
            .context("failed to get transaction info")
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

    pub(crate) fn get_mempool_entry(&self, txid: &Txid) -> Result<json::GetMempoolEntryResult> {
        self.rpc
            .get_mempool_entry(txid)
            .context("failed to get mempool entry")
    }

    pub(crate) fn get_new_headers(&self, chain: &Chain) -> Result<Vec<NewHeader>> {
        self.p2p.lock().get_new_headers(chain)
    }

    pub(crate) fn for_blocks<B, F>(&self, blockhashes: B, func: F) -> Result<()>
    where
        B: IntoIterator<Item = BlockHash>,
        F: FnMut(BlockHash, Block),
    {
        self.p2p.lock().for_blocks(blockhashes, func)
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
