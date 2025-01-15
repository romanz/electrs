use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::io::{BufRead, BufReader, Lines, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{env, fs, io};

use base64::prelude::{Engine, BASE64_STANDARD};
use bitcoin::hex::FromHex;
use error_chain::ChainedError;
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};
use serde_json::{from_str, from_value, Value};

#[cfg(not(feature = "liquid"))]
use bitcoin::consensus::encode::{deserialize, serialize_hex};
#[cfg(feature = "liquid")]
use elements::encode::{deserialize, serialize_hex};

use electrs_macros::trace;

use crate::chain::{Block, BlockHash, BlockHeader, Network, Transaction, Txid};
use crate::metrics::{HistogramOpts, HistogramVec, Metrics};
use crate::signal::Waiter;
use crate::util::{HeaderList, DEFAULT_BLOCKHASH};

use crate::errors::*;

lazy_static! {
    static ref DAEMON_CONNECTION_TIMEOUT: Duration = Duration::from_secs(
        env::var("DAEMON_CONNECTION_TIMEOUT").map_or(10, |s| s.parse().unwrap())
    );
    static ref DAEMON_READ_TIMEOUT: Duration = Duration::from_secs(
        env::var("DAEMON_READ_TIMEOUT").map_or(10 * 60, |s| s.parse().unwrap())
    );
    static ref DAEMON_WRITE_TIMEOUT: Duration = Duration::from_secs(
        env::var("DAEMON_WRITE_TIMEOUT").map_or(10 * 60, |s| s.parse().unwrap())
    );
}

const MAX_ATTEMPTS: u32 = 5;
const RETRY_WAIT_DURATION: Duration = Duration::from_secs(1);

#[trace]
fn parse_hash<T>(value: &Value) -> Result<T>
where
    T: FromStr,
    T::Err: 'static + std::error::Error + Send,
{
    Ok(T::from_str(
        value
            .as_str()
            .chain_err(|| format!("non-string value: {}", value))?,
    )
    .chain_err(|| format!("non-hex value: {}", value))?)
}

#[trace]
fn header_from_value(value: Value) -> Result<BlockHeader> {
    let header_hex = value
        .as_str()
        .chain_err(|| format!("non-string header: {}", value))?;
    let header_bytes = Vec::from_hex(header_hex).chain_err(|| "non-hex header")?;
    Ok(
        deserialize(&header_bytes)
            .chain_err(|| format!("failed to parse header {}", header_hex))?,
    )
}

fn block_from_value(value: Value) -> Result<Block> {
    let block_hex = value.as_str().chain_err(|| "non-string block")?;
    let block_bytes = Vec::from_hex(block_hex).chain_err(|| "non-hex block")?;
    Ok(deserialize(&block_bytes).chain_err(|| format!("failed to parse block {}", block_hex))?)
}

fn tx_from_value(value: Value) -> Result<Transaction> {
    let tx_hex = value.as_str().chain_err(|| "non-string tx")?;
    let tx_bytes = Vec::from_hex(tx_hex).chain_err(|| "non-hex tx")?;
    Ok(deserialize(&tx_bytes).chain_err(|| format!("failed to parse tx {}", tx_hex))?)
}

/// Parse JSONRPC error code, if exists.
fn parse_error_code(err: &Value) -> Option<i64> {
    err.as_object()?.get("code")?.as_i64()
}

fn parse_jsonrpc_reply(mut reply: Value, method: &str, expected_id: u64) -> Result<Value> {
    if let Some(reply_obj) = reply.as_object_mut() {
        if let Some(err) = reply_obj.get_mut("error") {
            if !err.is_null() {
                if let Some(code) = parse_error_code(&err) {
                    let msg = err["message"]
                        .as_str()
                        .map_or_else(|| err.to_string(), |s| s.to_string());
                    match code {
                        // RPC_IN_WARMUP -> retry by later reconnection
                        -28 => bail!(ErrorKind::Connection(err.to_string())),
                        code => bail!(ErrorKind::RpcError(code, msg, method.to_string())),
                    }
                }
            }
        }
        let id = reply_obj
            .get("id")
            .chain_err(|| format!("no id in reply: {:?}", reply_obj))?
            .clone();
        if id != expected_id {
            bail!(
                "wrong {} response id {}, expected {}",
                method,
                id,
                expected_id
            );
        }
        if let Some(result) = reply_obj.get_mut("result") {
            return Ok(result.take());
        }
        bail!("no result in reply: {:?}", reply_obj);
    }
    bail!("non-object reply: {:?}", reply);
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BlockchainInfo {
    pub chain: String,
    pub blocks: u32,
    pub headers: u32,
    pub bestblockhash: String,
    pub pruned: bool,
    pub verificationprogress: f32,
    pub initialblockdownload: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug)]
struct NetworkInfo {
    version: u64,
    subversion: String,
    relayfee: f64, // in BTC/kB
}

pub trait CookieGetter: Send + Sync {
    fn get(&self) -> Result<Vec<u8>>;
}

struct Connection {
    tx: TcpStream,
    rx: Lines<BufReader<TcpStream>>,
    cookie_getter: Arc<dyn CookieGetter>,
    addr: SocketAddr,
    signal: Waiter,
}

#[trace]
fn tcp_connect(addr: SocketAddr, signal: &Waiter) -> Result<TcpStream> {
    loop {
        match TcpStream::connect_timeout(&addr, *DAEMON_CONNECTION_TIMEOUT) {
            Ok(conn) => {
                // can only fail if DAEMON_TIMEOUT is 0
                conn.set_read_timeout(Some(*DAEMON_READ_TIMEOUT)).unwrap();
                conn.set_write_timeout(Some(*DAEMON_WRITE_TIMEOUT)).unwrap();
                return Ok(conn);
            }
            Err(err) => {
                warn!(
                    "failed to connect daemon at {}: {} (backoff 3 seconds)",
                    addr, err
                );
                signal.wait(Duration::from_secs(3), false)?;
                continue;
            }
        }
    }
}

impl Connection {
    #[trace]
    fn new(
        addr: SocketAddr,
        cookie_getter: Arc<dyn CookieGetter>,
        signal: Waiter,
    ) -> Result<Connection> {
        let conn = tcp_connect(addr, &signal)?;
        let reader = BufReader::new(
            conn.try_clone()
                .chain_err(|| format!("failed to clone {:?}", conn))?,
        );
        Ok(Connection {
            tx: conn,
            rx: reader.lines(),
            cookie_getter,
            addr,
            signal,
        })
    }

    #[trace]
    fn reconnect(&self) -> Result<Connection> {
        Connection::new(self.addr, self.cookie_getter.clone(), self.signal.clone())
    }

    #[trace]
    fn send(&mut self, request: &str) -> Result<()> {
        let cookie = &self.cookie_getter.get()?;
        let msg = format!(
            "POST / HTTP/1.1\nAuthorization: Basic {}\nContent-Length: {}\n\n{}",
            BASE64_STANDARD.encode(cookie),
            request.len(),
            request,
        );
        self.tx.write_all(msg.as_bytes()).chain_err(|| {
            ErrorKind::Connection("disconnected from daemon while sending".to_owned())
        })
    }

    #[trace]
    fn recv(&mut self) -> Result<String> {
        // TODO: use proper HTTP parser.
        let mut in_header = true;
        let mut contents: Option<String> = None;
        let iter = self.rx.by_ref();
        let status = iter
            .next()
            .chain_err(|| {
                ErrorKind::Connection("disconnected from daemon while receiving".to_owned())
            })?
            .chain_err(|| ErrorKind::Connection("failed to read status".to_owned()))?;
        let mut headers = HashMap::new();
        for line in iter {
            let line = line.chain_err(|| ErrorKind::Connection("failed to read".to_owned()))?;
            if line.is_empty() {
                in_header = false; // next line should contain the actual response.
            } else if in_header {
                let parts: Vec<&str> = line.splitn(2, ": ").collect();
                if parts.len() == 2 {
                    headers.insert(parts[0].to_owned(), parts[1].to_owned());
                } else {
                    warn!("invalid header: {:?}", line);
                }
            } else {
                contents = Some(line);
                break;
            }
        }

        let contents =
            contents.chain_err(|| ErrorKind::Connection("no reply from daemon".to_owned()))?;
        let contents_length: &str = headers
            .get("Content-Length")
            .chain_err(|| format!("Content-Length is missing: {:?}", headers))?;
        let contents_length: usize = contents_length
            .parse()
            .chain_err(|| format!("invalid Content-Length: {:?}", contents_length))?;

        let expected_length = contents_length - 1; // trailing EOL is skipped
        if expected_length != contents.len() {
            bail!(ErrorKind::Connection(format!(
                "expected {} bytes, got {}",
                expected_length,
                contents.len()
            )));
        }

        Ok(if status == "HTTP/1.1 200 OK" {
            contents
        } else if status == "HTTP/1.1 500 Internal Server Error" {
            debug!("RPC HTTP 500 error: {}", contents);
            contents // the contents should have a JSONRPC error field
        } else {
            bail!(
                "request failed {:?}: {:?} = {:?}",
                status,
                headers,
                contents
            );
        })
    }
}

struct Counter {
    value: Mutex<u64>,
}

impl Counter {
    fn new() -> Self {
        Counter {
            value: Mutex::new(0),
        }
    }

    fn next(&self) -> u64 {
        let mut value = self.value.lock().unwrap();
        *value += 1;
        *value
    }
}

pub struct Daemon {
    daemon_dir: PathBuf,
    blocks_dir: PathBuf,
    network: Network,
    conn: Mutex<Connection>,
    message_id: Counter, // for monotonic JSONRPC 'id'
    signal: Waiter,

    rpc_threads: Arc<rayon::ThreadPool>,

    // monitoring
    latency: HistogramVec,
    size: HistogramVec,
}

impl Daemon {
    pub fn new(
        daemon_dir: &PathBuf,
        blocks_dir: &PathBuf,
        daemon_rpc_addr: SocketAddr,
        daemon_parallelism: usize,
        cookie_getter: Arc<dyn CookieGetter>,
        network: Network,
        signal: Waiter,
        metrics: &Metrics,
    ) -> Result<Daemon> {
        let daemon = Daemon {
            daemon_dir: daemon_dir.clone(),
            blocks_dir: blocks_dir.clone(),
            network,
            conn: Mutex::new(Connection::new(
                daemon_rpc_addr,
                cookie_getter,
                signal.clone(),
            )?),
            message_id: Counter::new(),
            signal: signal.clone(),
            rpc_threads: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(daemon_parallelism)
                    .thread_name(|i| format!("rpc-requests-{}", i))
                    .build()
                    .unwrap(),
            ),
            latency: metrics.histogram_vec(
                HistogramOpts::new("daemon_rpc", "Bitcoind RPC latency (in seconds)"),
                &["method"],
            ),
            size: metrics.histogram_vec(
                HistogramOpts::new("daemon_bytes", "Bitcoind RPC size (in bytes)"),
                &["method", "dir"],
            ),
        };
        let network_info = daemon.getnetworkinfo()?;
        info!("{:?}", network_info);
        if network_info.version < 16_00_00 {
            bail!(
                "{} is not supported - please use bitcoind 0.16+",
                network_info.subversion,
            )
        }
        let blockchain_info = daemon.getblockchaininfo()?;
        info!("{:?}", blockchain_info);
        if blockchain_info.pruned {
            bail!("pruned node is not supported (use '-prune=0' bitcoind flag)".to_owned())
        }
        loop {
            let info = daemon.getblockchaininfo()?;

            if !info.initialblockdownload.unwrap_or(false) && info.blocks == info.headers {
                break;
            }

            warn!(
                "waiting for bitcoind sync to finish: {}/{} blocks, verification progress: {:.3}%",
                info.blocks,
                info.headers,
                info.verificationprogress * 100.0
            );
            signal.wait(Duration::from_secs(5), false)?;
        }
        Ok(daemon)
    }

    #[trace]
    pub fn reconnect(&self) -> Result<Daemon> {
        Ok(Daemon {
            daemon_dir: self.daemon_dir.clone(),
            blocks_dir: self.blocks_dir.clone(),
            network: self.network,
            conn: Mutex::new(self.conn.lock().unwrap().reconnect()?),
            message_id: Counter::new(),
            signal: self.signal.clone(),
            rpc_threads: self.rpc_threads.clone(),
            latency: self.latency.clone(),
            size: self.size.clone(),
        })
    }

    #[trace]
    pub fn list_blk_files(&self) -> Result<Vec<PathBuf>> {
        let path = self.blocks_dir.join("blk*.dat");
        debug!("listing block files at {:?}", path);
        let mut paths: Vec<PathBuf> = glob::glob(path.to_str().unwrap())
            .chain_err(|| "failed to list blk*.dat files")?
            .map(|res| res.unwrap())
            .collect();
        paths.sort();
        Ok(paths)
    }

    /// bitcoind v28.0+ defaults to xor-ing all blk*.dat files with this key,
    /// stored in the blocks dir.
    /// See: <https://github.com/bitcoin/bitcoin/pull/28052>
    pub fn read_blk_file_xor_key(&self) -> Result<Option<[u8; 8]>> {
        // From: <https://github.com/bitcoin/bitcoin/blob/v28.0/src/node/blockstorage.cpp#L1160>
        let path = self.blocks_dir.join("xor.dat");
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).chain_err(|| "failed to read daemon xor.dat file"),
        };
        let xor_key: [u8; 8] = <[u8; 8]>::try_from(bytes.as_slice()).chain_err(|| {
            format!(
                "xor.dat unexpected length: actual: {}, expected: 8",
                bytes.len()
            )
        })?;
        Ok(Some(xor_key))
    }

    pub fn magic(&self) -> u32 {
        self.network.magic()
    }

    #[trace]
    fn call_jsonrpc(&self, method: &str, request: &Value) -> Result<Value> {
        let mut conn = self.conn.lock().unwrap();
        let timer = self.latency.with_label_values(&[method]).start_timer();
        let request = request.to_string();
        conn.send(&request)?;
        self.size
            .with_label_values(&[method, "send"])
            .observe(request.len() as f64);
        let response = conn.recv()?;
        let result: Value = from_str(&response).chain_err(|| "invalid JSON")?;
        timer.observe_duration();
        self.size
            .with_label_values(&[method, "recv"])
            .observe(response.len() as f64);
        Ok(result)
    }

    #[trace(method = %method)]
    fn handle_request(&self, method: &str, params: &Value) -> Result<Value> {
        let id = self.message_id.next();
        let req = json!({"method": method, "params": params, "id": id});
        let reply = self.call_jsonrpc(method, &req)?;
        parse_jsonrpc_reply(reply, method, id)
    }

    fn retry_request(&self, method: &str, params: &Value) -> Result<Value> {
        loop {
            match self.handle_request(method, &params) {
                Err(e @ Error(ErrorKind::Connection(_), _)) => {
                    warn!("reconnecting to bitcoind: {}", e.display_chain());
                    self.signal.wait(Duration::from_secs(3), false)?;
                    let mut conn = self.conn.lock().unwrap();
                    *conn = conn.reconnect()?;
                    continue;
                }
                result => return result,
            }
        }
    }

    #[trace]
    fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.retry_request(method, &params)
    }

    #[trace]
    fn retry_reconnect(&self) -> Daemon {
        // XXX add a max reconnection attempts limit?
        loop {
            match self.reconnect() {
                Ok(daemon) => break daemon,
                Err(e) => {
                    warn!("failed connecting to RPC daemon: {}", e.display_chain());
                }
            }
        }
    }

    // Send requests in parallel over multiple RPC connections as individual JSON-RPC requests (with no JSON-RPC batching),
    // buffering the replies into a vector. If any of the requests fail, processing is terminated and an Err is returned.
    #[trace]
    fn requests(&self, method: &str, params_list: Vec<Value>) -> Result<Vec<Value>> {
        self.requests_iter(method, params_list).collect()
    }

    // Send requests in parallel over multiple RPC connections, iterating over the results without buffering them.
    // Errors are included in the iterator and do not terminate other pending requests.
    #[trace]
    fn requests_iter<'a>(
        &'a self,
        method: &'a str,
        params_list: Vec<Value>,
    ) -> impl ParallelIterator<Item = Result<Value>> + IndexedParallelIterator + 'a {
        self.rpc_threads.install(move || {
            params_list.into_par_iter().map(move |params| {
                // Store a local per-thread Daemon, each with its own TCP connection. These will
                // get initialized as necessary for the `rpc_threads` pool thread managed by rayon.
                thread_local!(static DAEMON_INSTANCE: OnceCell<Daemon> = OnceCell::new());

                DAEMON_INSTANCE.with(|daemon| {
                    daemon
                        .get_or_init(|| self.retry_reconnect())
                        .retry_request(&method, &params)
                })
            })
        })
    }

    // bitcoind JSONRPC API:

    #[trace]
    pub fn getblockchaininfo(&self) -> Result<BlockchainInfo> {
        let info: Value = self.request("getblockchaininfo", json!([]))?;
        Ok(from_value(info).chain_err(|| "invalid blockchain info")?)
    }

    #[trace]
    fn getnetworkinfo(&self) -> Result<NetworkInfo> {
        let info: Value = self.request("getnetworkinfo", json!([]))?;
        Ok(from_value(info).chain_err(|| "invalid network info")?)
    }

    #[trace]
    pub fn getbestblockhash(&self) -> Result<BlockHash> {
        parse_hash(&self.request("getbestblockhash", json!([]))?)
    }

    #[trace]
    pub fn getblockheader(&self, blockhash: &BlockHash) -> Result<BlockHeader> {
        header_from_value(self.request("getblockheader", json!([blockhash, /*verbose=*/ false]))?)
    }

    #[trace]
    pub fn getblockheaders(&self, heights: &[usize]) -> Result<Vec<BlockHeader>> {
        let heights: Vec<Value> = heights.iter().map(|height| json!([height])).collect();
        let params_list: Vec<Value> = self
            .requests("getblockhash", heights)?
            .into_iter()
            .map(|hash| json!([hash, /*verbose=*/ false]))
            .collect();
        let mut result = vec![];
        for h in self.requests("getblockheader", params_list)? {
            result.push(header_from_value(h)?);
        }
        Ok(result)
    }

    #[trace]
    pub fn getblock(&self, blockhash: &BlockHash) -> Result<Block> {
        let block =
            block_from_value(self.request("getblock", json!([blockhash, /*verbose=*/ false]))?)?;
        assert_eq!(block.block_hash(), *blockhash);
        Ok(block)
    }

    #[trace]
    pub fn getblock_raw(&self, blockhash: &BlockHash, verbose: u32) -> Result<Value> {
        self.request("getblock", json!([blockhash, verbose]))
    }

    #[trace]
    pub fn getblocks(&self, blockhashes: &[BlockHash]) -> Result<Vec<Block>> {
        let params_list: Vec<Value> = blockhashes
            .iter()
            .map(|hash| json!([hash, /*verbose=*/ false]))
            .collect();

        let mut attempts = MAX_ATTEMPTS;
        let values = loop {
            attempts -= 1;

            match self.requests("getblock", params_list.clone()) {
                Ok(blocks) => break blocks,
                Err(e) => {
                    let err_msg = format!("{e:?}");
                    if err_msg.contains("Block not found on disk") {
                        // There is a small chance the node returns the header but didn't finish to index the block
                        log::warn!("getblocks failing with: {e:?} trying {attempts} more time")
                    } else {
                        panic!("failed to get blocks from bitcoind: {}", err_msg);
                    }
                }
            }
            if attempts == 0 {
                panic!("failed to get blocks from bitcoind")
            }
            std::thread::sleep(RETRY_WAIT_DURATION);
        };
        let mut blocks = vec![];
        for value in values {
            blocks.push(block_from_value(value)?);
        }
        Ok(blocks)
    }

    /// Fetch the given transactions in parallel over multiple threads and RPC connections,
    /// ignoring any missing ones and returning whatever is available.
    #[trace]
    pub fn gettransactions_available(&self, txids: &[&Txid]) -> Result<Vec<(Txid, Transaction)>> {
        const RPC_INVALID_ADDRESS_OR_KEY: i64 = -5;

        let params_list: Vec<Value> = txids
            .iter()
            .map(|txhash| json!([txhash, /*verbose=*/ false]))
            .collect();

        self.requests_iter("getrawtransaction", params_list)
            .zip(txids)
            .filter_map(|(res, txid)| match res {
                Ok(val) => Some(tx_from_value(val).map(|tx| (**txid, tx))),
                // Ignore 'tx not found' errors
                Err(Error(ErrorKind::RpcError(code, _, _), _))
                    if code == RPC_INVALID_ADDRESS_OR_KEY =>
                {
                    None
                }
                // Terminate iteration if any other errors are encountered
                Err(e) => Some(Err(e)),
            })
            .collect()
    }

    #[trace]
    pub fn gettransaction_raw(
        &self,
        txid: &Txid,
        blockhash: &BlockHash,
        verbose: bool,
    ) -> Result<Value> {
        self.request("getrawtransaction", json!([txid, verbose, blockhash]))
    }

    #[trace]
    pub fn getmempooltx(&self, txhash: &Txid) -> Result<Transaction> {
        let value = self.request("getrawtransaction", json!([txhash, /*verbose=*/ false]))?;
        tx_from_value(value)
    }

    #[trace]
    pub fn getmempooltxids(&self) -> Result<HashSet<Txid>> {
        let res = self.request("getrawmempool", json!([/*verbose=*/ false]))?;
        Ok(serde_json::from_value(res).chain_err(|| "invalid getrawmempool reply")?)
    }

    #[trace]
    pub fn broadcast(&self, tx: &Transaction) -> Result<Txid> {
        self.broadcast_raw(&serialize_hex(tx))
    }

    #[trace]
    pub fn broadcast_raw(&self, txhex: &str) -> Result<Txid> {
        let txid = self.request("sendrawtransaction", json!([txhex]))?;
        Ok(
            Txid::from_str(txid.as_str().chain_err(|| "non-string txid")?)
                .chain_err(|| "failed to parse txid")?,
        )
    }

    // Get estimated feerates for the provided confirmation targets using a batch RPC request
    // Missing estimates are logged but do not cause a failure, whatever is available is returned
    #[allow(clippy::float_cmp)]
    #[trace]
    pub fn estimatesmartfee_batch(&self, conf_targets: &[u16]) -> Result<HashMap<u16, f64>> {
        let params_list: Vec<Value> = conf_targets
            .iter()
            .map(|t| json!([t, "ECONOMICAL"]))
            .collect();

        Ok(self
            .requests("estimatesmartfee", params_list)?
            .iter()
            .zip(conf_targets)
            .filter_map(|(reply, target)| {
                if !reply["errors"].is_null() {
                    warn!(
                        "failed estimating fee for target {}: {:?}",
                        target, reply["errors"]
                    );
                    return None;
                }

                let feerate = reply["feerate"]
                    .as_f64()
                    .unwrap_or_else(|| panic!("invalid estimatesmartfee response: {:?}", reply));

                if feerate == -1f64 {
                    warn!("not enough data to estimate fee for target {}", target);
                    return None;
                }

                // from BTC/kB to sat/b
                Some((*target, feerate * 100_000f64))
            })
            .collect())
    }

    #[trace]
    fn get_all_headers(&self, tip: &BlockHash) -> Result<Vec<BlockHeader>> {
        let info: Value = self.request("getblockheader", json!([tip]))?;
        let tip_height = info
            .get("height")
            .expect("missing height")
            .as_u64()
            .expect("non-numeric height") as usize;
        let all_heights: Vec<usize> = (0..=tip_height).collect();
        let chunk_size = 100_000;
        let mut result = vec![];
        for heights in all_heights.chunks(chunk_size) {
            trace!("downloading {} block headers", heights.len());
            let mut headers = self.getblockheaders(&heights)?;
            assert!(headers.len() == heights.len());
            result.append(&mut headers);
        }

        let mut blockhash = *DEFAULT_BLOCKHASH;
        for header in &result {
            assert_eq!(header.prev_blockhash, blockhash);
            blockhash = header.block_hash();
        }
        assert_eq!(blockhash, *tip);
        Ok(result)
    }

    // Returns a list of BlockHeaders in ascending height (i.e. the tip is last).
    #[trace]
    pub fn get_new_headers(
        &self,
        indexed_headers: &HeaderList,
        bestblockhash: &BlockHash,
    ) -> Result<Vec<BlockHeader>> {
        // Iterate back over headers until known blockash is found:
        if indexed_headers.is_empty() {
            debug!("downloading all block headers up to {}", bestblockhash);
            return self.get_all_headers(bestblockhash);
        }
        debug!(
            "downloading new block headers ({} already indexed) from {}",
            indexed_headers.len(),
            bestblockhash,
        );
        let mut new_headers = vec![];
        let mut blockhash = *bestblockhash;
        while blockhash != *DEFAULT_BLOCKHASH {
            if indexed_headers.header_by_blockhash(&blockhash).is_some() {
                break;
            }
            let header = self
                .getblockheader(&blockhash)
                .chain_err(|| format!("failed to get {} header", blockhash))?;
            blockhash = header.prev_blockhash;
            new_headers.push(header);
        }
        trace!("downloaded {} block headers", new_headers.len());
        new_headers.reverse(); // so the tip is the last vector entry
        Ok(new_headers)
    }

    #[trace]
    pub fn get_relayfee(&self) -> Result<f64> {
        let relayfee = self.getnetworkinfo()?.relayfee;

        // from BTC/kB to sat/b
        Ok(relayfee * 100_000f64)
    }
}
