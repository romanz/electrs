use anyhow::{Context, Result};

use bitcoin::consensus::encode::serialize_hex;
use bitcoin::{consensus::deserialize, hashes::hex::FromHex};
use bitcoin::{Amount, BlockHash, Transaction, Txid};
use bitcoincore_rpc::{json, jsonrpc, Auth, Client, RpcApi};
use crossbeam_channel::Receiver;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, value::RawValue, Value};

use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
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
    // Allow RPC calls to take longer before timing out.
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

        let p2p = Connection::connect(config.daemon_p2p_addr, metrics, config.magic)?;
        warn_if_not_whitelisted(&rpc, p2p.local_addr());
        let p2p = Mutex::new(p2p);
        Ok(Self { p2p, rpc })
    }

    pub(crate) fn estimate_fee(&self, nblocks: u16) -> Result<Option<Amount>> {
        let res = self.rpc.estimate_smart_fee(nblocks, None);
        if let Err(bitcoincore_rpc::Error::JsonRpc(jsonrpc::Error::Rpc(RpcError {
            code: -32603,
            ..
        }))) = res
        {
            return Ok(None); // don't fail when fee estimation is disabled (e.g. with `-blocksonly=1`)
        }
        Ok(res.context("failed to estimate fee")?.fee_rate)
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

    pub(crate) fn submitpackage(&self, txs: &[Transaction]) -> Result<Value> {
        let package: Vec<String> = txs.iter().map(serialize_hex).collect();
        self.rpc
            .call("submitpackage", &[json!(package)])
            .context("failed to submitpackage package")
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

    pub(crate) fn get_mempool_info(&self) -> Result<json::GetMempoolInfoResult> {
        self.rpc
            .get_mempool_info()
            .context("failed to get mempool info")
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

#[derive(Debug, PartialEq, Eq)]
enum WhitelistStatus {
    Whitelisted,
    NotWhitelisted,
    Unknown(&'static str),
}

fn canonicalize(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// Inspect a `getpeerinfo` result and report whether the peer matching `local_addr`
/// has the `download` permission. Pure function for unit testing.
fn check_whitelist_status(peers: &[Value], local_addr: SocketAddr) -> WhitelistStatus {
    let target = canonicalize(local_addr);
    for peer in peers {
        let Some(addr_str) = peer.get("addr").and_then(Value::as_str) else {
            continue;
        };
        let Ok(addr) = addr_str.parse::<SocketAddr>() else {
            continue;
        };
        if canonicalize(addr) != target {
            continue;
        }
        // Skip peers bitcoind connected out to; electrs is always inbound from its POV.
        if peer.get("inbound").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let Some(perms) = peer.get("permissions").and_then(Value::as_array) else {
            return WhitelistStatus::Unknown(
                "matched peer has no `permissions` field (bitcoind too old?)",
            );
        };
        let has_download = perms.iter().any(|p| p.as_str() == Some("download"));
        return if has_download {
            WhitelistStatus::Whitelisted
        } else {
            WhitelistStatus::NotWhitelisted
        };
    }
    WhitelistStatus::Unknown("no matching peer in getpeerinfo output")
}

/// Log a warning if bitcoind reports that electrs is connected without `download` permission.
/// Missing whitelisting can cause bitcoind to disconnect electrs or ignore `getheaders`
/// during IBD or once `maxuploadtarget` is reached.
/// See https://github.com/romanz/electrs/issues/400 for background.
fn warn_if_not_whitelisted(rpc: &Client, local_addr: SocketAddr) {
    let peers: Value = match rpc.call("getpeerinfo", &[]) {
        Ok(v) => v,
        Err(err) => {
            debug!("getpeerinfo failed, skipping whitelist check: {}", err);
            return;
        }
    };
    let Some(peers) = peers.as_array() else {
        debug!("getpeerinfo returned non-array, skipping whitelist check");
        return;
    };
    match check_whitelist_status(peers, local_addr) {
        WhitelistStatus::Whitelisted => debug!("electrs is whitelisted by bitcoind"),
        WhitelistStatus::NotWhitelisted => warn!(
            "electrs is not whitelisted by bitcoind: it may be disconnected during IBD \
             or once `maxuploadtarget` is reached. Consider adding `whitelist=download@127.0.0.1` \
             to bitcoin.conf. See https://github.com/romanz/electrs/issues/400 for details."
        ),
        WhitelistStatus::Unknown(reason) => debug!("whitelist check inconclusive: {}", reason),
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
    let args: Vec<Box<RawValue>> = items
        .iter()
        .map(|item| jsonrpc::try_arg([item]).context("failed to serialize into JSON"))
        .collect::<Result<Vec<_>>>()?;
    let reqs: Vec<jsonrpc::Request> = args
        .iter()
        .map(|arg| client.build_request(name, Some(arg)))
        .collect();
    match client.send_batch(&reqs) {
        Ok(values) => {
            assert_eq!(items.len(), values.len());
            Ok(values)
        }
        Err(err) => bail!("batch {} request failed: {}", name, err),
    }
}

#[cfg(test)]
mod tests {
    use super::{check_whitelist_status, WhitelistStatus};
    use serde_json::json;
    use std::net::SocketAddr;

    fn local() -> SocketAddr {
        "127.0.0.1:38234".parse().unwrap()
    }

    #[test]
    fn whitelisted_when_download_permission_present() {
        let peers = vec![json!({
            "addr": "127.0.0.1:38234",
            "inbound": true,
            "permissions": ["noban", "download", "mempool"]
        })];
        assert_eq!(
            check_whitelist_status(&peers, local()),
            WhitelistStatus::Whitelisted
        );
    }

    #[test]
    fn not_whitelisted_when_download_permission_missing() {
        let peers = vec![json!({
            "addr": "127.0.0.1:38234",
            "inbound": true,
            "permissions": ["relay"]
        })];
        assert_eq!(
            check_whitelist_status(&peers, local()),
            WhitelistStatus::NotWhitelisted
        );
    }

    #[test]
    fn unknown_when_no_matching_peer() {
        let peers = vec![json!({
            "addr": "10.0.0.5:8333",
            "inbound": false,
            "permissions": ["download"]
        })];
        assert!(matches!(
            check_whitelist_status(&peers, local()),
            WhitelistStatus::Unknown(_)
        ));
    }

    #[test]
    fn unknown_when_permissions_field_missing() {
        let peers = vec![json!({
            "addr": "127.0.0.1:38234",
            "inbound": true,
        })];
        assert!(matches!(
            check_whitelist_status(&peers, local()),
            WhitelistStatus::Unknown(_)
        ));
    }

    #[test]
    fn matches_ipv4_mapped_ipv6_addr() {
        // bitcoind may report the peer as an IPv4-mapped IPv6 address on dual-stack systems.
        let peers = vec![json!({
            "addr": "[::ffff:127.0.0.1]:38234",
            "inbound": true,
            "permissions": ["download"]
        })];
        assert_eq!(
            check_whitelist_status(&peers, local()),
            WhitelistStatus::Whitelisted
        );
    }

    #[test]
    fn skips_outbound_peers_with_matching_addr() {
        // Defensive: an outbound peer at the same addr can't be electrs.
        let peers = vec![
            json!({
                "addr": "127.0.0.1:38234",
                "inbound": false,
                "permissions": ["download"]
            }),
            json!({
                "addr": "127.0.0.1:38234",
                "inbound": true,
                "permissions": []
            }),
        ];
        assert_eq!(
            check_whitelist_status(&peers, local()),
            WhitelistStatus::NotWhitelisted
        );
    }

    #[test]
    fn ignores_unrelated_peers() {
        let peers = vec![
            json!({"addr": "1.2.3.4:8333", "inbound": false, "permissions": []}),
            json!({"addr": "127.0.0.1:38234", "inbound": true, "permissions": ["download"]}),
        ];
        assert_eq!(
            check_whitelist_status(&peers, local()),
            WhitelistStatus::Whitelisted
        );
    }

    #[test]
    fn returns_unknown_on_empty_peer_list() {
        assert!(matches!(
            check_whitelist_status(&[], local()),
            WhitelistStatus::Unknown(_)
        ));
    }
}
