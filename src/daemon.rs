use anyhow::{Context, Result};

use bitcoin::{consensus::deserialize, hashes::hex::FromHex};
use bitcoin::{Amount, BlockHash, Transaction, Txid};
use bitcoincore_rpc::{json, jsonrpc, Auth, Client, RpcApi};
use crossbeam_channel::Receiver;
use parking_lot::Mutex;
use serde::Serialize;
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
    ) -> Result<Vec<Option<json::GetMempoolEntryResult>>> {
        let results = batch_request(self.rpc.get_jsonrpc_client(), "getmempoolentry", txids)?;
        Ok(results
            .into_iter()
            .map(|r| match r?.result::<json::GetMempoolEntryResult>() {
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
        let results = batch_request(self.rpc.get_jsonrpc_client(), "getrawtransaction", txids)?;
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

fn batch_request<T>(
    client: &jsonrpc::Client,
    name: &str,
    items: &[T],
) -> Result<Vec<Option<jsonrpc::Response>>>
where
    T: Serialize,
{
    debug!("calling {} on {} items", name, items.len());
    let args: Vec<_> = items
        .iter()
        .map(|item| vec![serde_json::value::to_raw_value(item).unwrap()])
        .collect();
    let reqs: Vec<_> = args
        .iter()
        .map(|arg| client.build_request(name, arg))
        .collect();
    match client.send_batch(&reqs) {
        Ok(values) => {
            assert_eq!(items.len(), values.len());
            Ok(values)
        }
        Err(err) => bail!("batch {} request failed: {}", name, err),
    }
}

macro_rules! define_error_codes {
    ($($(#[doc = $doc:expr])* $name:ident = $value:expr,)*) => {
        /// Bitcoin Core RPC error codes
        /// * https://github.com/bitcoin/bitcoin/blob/master/src/rpc/protocol.h
        #[derive(Debug, Clone, Copy)]
        #[repr(i32)]
        pub(crate) enum CoreRPCErrorCode {
            $(
                $(#[doc = $doc])*
                $name = $value,
            )*
        }

        impl CoreRPCErrorCode {
            pub(crate) fn from_error_code(code: i32) -> Option<Self> {
                Some(match code {
                    $(
                        $value => Self::$name,
                    )*
                    _ => return None,
                })
            }
        }
    }
}

define_error_codes! {
    /// RpcInvalidRequest is mapped to HTTP_BAD_REQUEST (400).
    /// It is not used for application-layer errors
    RpcInvalidRequest = -32600,
    /// RpcMethodNotFound is mapped to HTTP_NOT_FOUND (404).
    /// It is not be used for application-layer errors.
    RpcMethodNotFound = -32601,
    RpcInvalidParams = -32602,
    /// RpcInternalError is used for genuine errors in bitcoind
    RpcInternalError = -32603,
    RpcParseError = -32700,

    /// std::exception thrown in command handling
    RpcMiscError = -1,
    /// Unexpected type was passed as parameter
    RpcTypeError = -3,
    /// Invalid address or key
    RpcInvalidAddressOrKey = -5,
    /// Ran out of memory during operation
    RpcOutOfMemory = -7,
    /// Invalid, missing or duplicate parameter
    RpcInvalidParameter = -8,
    /// Database error
    RpcDatabaseError = -20,
    /// Error parsing or validating structure in raw format
    RpcDeserializationError = -22,
    /// General error during transaction or block submission
    RpcVerifyError = -25,
    /// Transaction or block was rejected by network rules
    RpcVerifyRejected = -26,
    /// Transaction already in chain
    RpcVerifyAlreadyInChain = -27,
    /// Client still warming up
    RpcInWarmup = -28,
    /// RPC method is deprecated
    RpcMethodDeprecated = -32,

    /// Bitcoin is not connected
    RpcClientNotConnected = -9,
    /// Still downloading initial blocks
    RpcClientInInitialDownload = -10,
    /// Node is already added
    RpcClientNodeAlreadyAdded = -23,
    /// Node has not been added before
    RpcClientNodeNotAdded = -24,
    /// Node to disconnect not found in connected nodes
    RpcClientNodeNotConnected = -29,
    /// Invalid IP/Subnet
    RpcClientInvalidIpOrSubnet = -30,
    /// No valid connection manager instance found
    RpcClientP2pDisabled = -31,
    /// Max number of outbound or block-relay connections already open
    RpcClientNodeCapacityReached = -34,
    /// No mempool instance found
    RpcClientMempoolDisabled = -33,

    /// Unspecified problem with wallet (key not found etc.)
    RpcWalletError = -4,
    /// Not enough funds in wallet or account
    RpcWalletInsufficientFunds = -6,
    /// Invalid label name
    RpcWalletInvalidLabelName = -11,
    /// Keypool ran out, call keypoolrefill first
    RpcWalletKeypoolRanOut = -12,
    /// Enter the wallet passphrase with walletpassphrase
    RpcWalletUnlockNeeded = -13,
    /// The wallet passphrase entered was incorrect
    RpcWalletPassphraseIncorrect = -14,
    /// Command given in wrong wallet encryption state (encrypting an encrypted wallet etc.)
    RpcWalletWrongEncState = -15,
    /// Failed to encrypt the wallet
    RpcWalletEncryptionFailed = -16,
    /// Wallet is already unlocked
    RpcWalletAlreadyUnlocked = -17,
    /// Invalid wallet specified
    RpcWalletNotFound = -18,
    /// No wallet specified (error when there are multiple wallets loaded)
    RpcWalletNotSpecified = -19,
    /// This same wallet is already loaded
    RpcWalletAlreadyLoaded = -35,
    /// There is already a wallet with the same name
    RpcWalletAlreadyExisits = -36,
}
