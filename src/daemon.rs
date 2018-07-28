use base64;
use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::blockdata::transaction::Transaction;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::{deserialize, serialize};
use bitcoin::util::hash::Sha256dHash;
use glob;
use hex;
use serde_json::{from_str, from_value, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Lines, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use metrics::{HistogramOpts, HistogramVec, Metrics};
use util::HeaderList;

use errors::*;

#[derive(Debug, Copy, Clone)]
pub enum Network {
    Mainnet,
    Testnet,
    Regtest,
}

fn parse_hash(value: &Value) -> Result<Sha256dHash> {
    Ok(Sha256dHash::from_hex(value
        .as_str()
        .chain_err(|| format!("non-string value: {}", value))?)
        .chain_err(|| {
        format!("non-hex value: {}", value)
    })?)
}

fn header_from_value(value: Value) -> Result<BlockHeader> {
    let header_hex = value
        .as_str()
        .chain_err(|| format!("non-string header: {}", value))?;
    let header_bytes = hex::decode(header_hex).chain_err(|| "non-hex header")?;
    Ok(deserialize(&header_bytes).chain_err(|| format!("failed to parse header {}", header_hex))?)
}

fn block_from_value(value: Value) -> Result<Block> {
    let block_hex = value.as_str().chain_err(|| "non-string block")?;
    let block_bytes = hex::decode(block_hex).chain_err(|| "non-hex block")?;
    Ok(deserialize(&block_bytes).chain_err(|| format!("failed to parse block {}", block_hex))?)
}

fn tx_from_value(value: Value) -> Result<Transaction> {
    let tx_hex = value.as_str().chain_err(|| "non-string tx")?;
    let tx_bytes = hex::decode(tx_hex).chain_err(|| "non-hex tx")?;
    Ok(deserialize(&tx_bytes).chain_err(|| format!("failed to parse tx {}", tx_hex))?)
}

fn parse_jsonrpc_reply(mut reply: Value, method: &str, expected_id: u64) -> Result<Value> {
    if let Some(reply_obj) = reply.as_object_mut() {
        if let Some(err) = reply_obj.get("error") {
            if !err.is_null() {
                bail!("{} RPC error: {}", method, err);
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
    chain: String,
    blocks: u32,
    headers: u32,
    bestblockhash: String,
    size_on_disk: u64,
    pruned: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct NetworkInfo {
    version: u64,
    subversion: String,
}

pub struct MempoolEntry {
    fee: u64,   // in satoshis
    vsize: u32, // in virtual bytes (= weight/4)
    fee_per_vbyte: f32,
}

impl MempoolEntry {
    fn new(fee: u64, vsize: u32) -> MempoolEntry {
        MempoolEntry {
            fee,
            vsize,
            fee_per_vbyte: fee as f32 / vsize as f32,
        }
    }

    pub fn fee_per_vbyte(&self) -> f32 {
        self.fee_per_vbyte
    }

    pub fn fee(&self) -> u64 {
        self.fee
    }

    pub fn vsize(&self) -> u32 {
        self.vsize
    }
}

pub trait CookieGetter: Send + Sync {
    fn get(&self) -> String;
}

struct Connection {
    tx: TcpStream,
    rx: Lines<BufReader<TcpStream>>,
    cookie_getter: Arc<CookieGetter>,
    addr: SocketAddr,
}

fn tcp_connect(addr: SocketAddr) -> Result<TcpStream> {
    loop {
        match TcpStream::connect(addr) {
            Ok(conn) => return Ok(conn),
            Err(err) => {
                warn!("failed to connect daemon at {}: {}", addr, err);
                thread::sleep(Duration::from_secs(3));
                continue;
            }
        }
    }
}

impl Connection {
    fn new(addr: SocketAddr, cookie_getter: Arc<CookieGetter>) -> Result<Connection> {
        let conn = tcp_connect(addr)?;
        let reader = BufReader::new(conn.try_clone()
            .chain_err(|| format!("failed to clone {:?}", conn))?);
        Ok(Connection {
            tx: conn,
            rx: reader.lines(),
            cookie_getter,
            addr,
        })
    }

    pub fn reconnect(&self) -> Result<Connection> {
        Connection::new(self.addr, self.cookie_getter.clone())
    }

    fn send(&mut self, request: &str) -> Result<()> {
        let cookie_b64 = base64::encode(&self.cookie_getter.get());
        let msg = format!(
            "POST / HTTP/1.1\nAuthorization: Basic {}\nContent-Length: {}\n\n{}",
            cookie_b64,
            request.len(),
            request,
        );
        self.tx.write_all(msg.as_bytes()).chain_err(|| {
            ErrorKind::Connection("disconnected from daemon while sending".to_owned())
        })
    }

    fn recv(&mut self) -> Result<String> {
        // TODO: use proper HTTP parser.
        let mut in_header = true;
        let mut contents: Option<String> = None;
        let iter = self.rx.by_ref();
        let status = iter.next()
            .chain_err(|| {
                ErrorKind::Connection("disconnected from daemon while receiving".to_owned())
            })?
            .chain_err(|| "failed to read status")?;
        if status != "HTTP/1.1 200 OK" {
            let msg = format!("request failed {:?}", status);
            bail!(ErrorKind::Connection(msg));
        }
        for line in iter {
            let line = line.chain_err(|| ErrorKind::Connection("failed to read".to_owned()))?;
            if line.is_empty() {
                in_header = false; // next line should contain the actual response.
            } else if !in_header {
                contents = Some(line);
                break;
            }
        }
        contents.chain_err(|| ErrorKind::Connection("no reply from daemon".to_owned()))
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
    network: Network,
    conn: Mutex<Connection>,
    message_id: Counter, // for monotonic JSONRPC 'id'

    // monitoring
    latency: HistogramVec,
    size: HistogramVec,
}

impl Daemon {
    pub fn new(
        daemon_dir: &PathBuf,
        daemon_rpc_addr: SocketAddr,
        cookie_getter: Arc<CookieGetter>,
        network: Network,
        metrics: &Metrics,
    ) -> Result<Daemon> {
        let daemon = Daemon {
            daemon_dir: daemon_dir.clone(),
            network,
            conn: Mutex::new(Connection::new(daemon_rpc_addr, cookie_getter)?),
            message_id: Counter::new(),
            latency: metrics.histogram_vec(
                HistogramOpts::new("daemon_rpc", "Bitcoind RPC latency (in seconds)"),
                &["method"],
            ),
            size: metrics.histogram_vec(
                HistogramOpts::new("daemon_bytes", "Bitcoind RPC size (in bytes)"),
                &["method", "dir"],
            ),
        };
        debug!("{:?}", daemon.getblockchaininfo()?);
        debug!("{:?}", daemon.getnetworkinfo()?);
        Ok(daemon)
    }

    pub fn reconnect(&self) -> Result<Daemon> {
        Ok(Daemon {
            daemon_dir: self.daemon_dir.clone(),
            network: self.network,
            conn: Mutex::new(self.conn.lock().unwrap().reconnect()?),
            message_id: Counter::new(),
            latency: self.latency.clone(),
            size: self.size.clone(),
        })
    }

    pub fn list_blk_files(&self) -> Result<Vec<PathBuf>> {
        let mut path = self.daemon_dir.clone();
        path.push("blocks");
        path.push("blk*.dat");
        info!("listing block files at {:?}", path);
        let mut paths: Vec<PathBuf> = glob::glob(path.to_str().unwrap())
            .chain_err(|| "failed to list blk*.dat files")?
            .map(|res| res.unwrap())
            .collect();
        paths.sort();
        Ok(paths)
    }

    pub fn magic(&self) -> u32 {
        match self.network {
            Network::Mainnet => 0xD9B4BEF9,
            Network::Testnet => 0x0709110B,
            Network::Regtest => 0xDAB5BFFA,
        }
    }

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

    fn retry_call_jsonrpc(&self, method: &str, request: &Value) -> Result<Value> {
        loop {
            match self.call_jsonrpc(method, request) {
                Err(Error(ErrorKind::Connection(msg), _)) => {
                    warn!("connection failed: {}", msg);
                    thread::sleep(Duration::from_secs(3));
                    let mut conn = self.conn.lock().unwrap();
                    *conn = conn.reconnect()?;
                    continue;
                }
                result => return result,
            }
        }
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.message_id.next();
        let req = json!({"method": method, "params": params, "id": id});
        let reply = self.retry_call_jsonrpc(method, &req)
            .chain_err(|| format!("RPC failed: {}", req))?;
        parse_jsonrpc_reply(reply, method, id)
    }

    fn requests(&self, method: &str, params_list: &[Value]) -> Result<Vec<Value>> {
        let id = self.message_id.next();
        let reqs = params_list
            .iter()
            .map(|params| json!({"method": method, "params": params, "id": id}))
            .collect();
        let mut results = vec![];
        let mut replies = self.retry_call_jsonrpc(method, &reqs)
            .chain_err(|| format!("RPC failed: {}", reqs))?;
        if let Some(replies_vec) = replies.as_array_mut() {
            for reply in replies_vec {
                results.push(parse_jsonrpc_reply(reply.take(), method, id)?)
            }
            return Ok(results);
        }
        bail!("non-array replies: {:?}", replies);
    }

    // bitcoind JSONRPC API:

    pub fn getblockchaininfo(&self) -> Result<BlockchainInfo> {
        let info: Value = self.request("getblockchaininfo", json!([]))?;
        Ok(from_value(info).chain_err(|| "invalid blockchain info")?)
    }

    pub fn getnetworkinfo(&self) -> Result<NetworkInfo> {
        let info: Value = self.request("getnetworkinfo", json!([]))?;
        Ok(from_value(info).chain_err(|| "invalid network info")?)
    }

    pub fn getbestblockhash(&self) -> Result<Sha256dHash> {
        parse_hash(&self.request("getbestblockhash", json!([]))?).chain_err(|| "invalid blockhash")
    }

    pub fn getblockheader(&self, blockhash: &Sha256dHash) -> Result<BlockHeader> {
        header_from_value(self.request(
            "getblockheader",
            json!([blockhash.be_hex_string(), /*verbose=*/ false]),
        )?)
    }

    pub fn getblockheaders(&self, heights: &[usize]) -> Result<Vec<BlockHeader>> {
        let heights: Vec<Value> = heights.iter().map(|height| json!([height])).collect();
        let params_list: Vec<Value> = self.requests("getblockhash", &heights)?
            .into_iter()
            .map(|hash| json!([hash, /*verbose=*/ false]))
            .collect();
        let mut result = vec![];
        for h in self.requests("getblockheader", &params_list)? {
            result.push(header_from_value(h)?);
        }
        Ok(result)
    }

    pub fn getblock(&self, blockhash: &Sha256dHash) -> Result<Block> {
        let block = block_from_value(self.request(
            "getblock",
            json!([blockhash.be_hex_string(), /*verbose=*/ false]),
        )?)?;
        assert_eq!(block.bitcoin_hash(), *blockhash);
        Ok(block)
    }

    pub fn getblocks(&self, blockhashes: &[Sha256dHash]) -> Result<Vec<Block>> {
        let params_list: Vec<Value> = blockhashes
            .iter()
            .map(|hash| json!([hash.be_hex_string(), /*verbose=*/ false]))
            .collect();
        let values = self.requests("getblock", &params_list)?;
        let mut blocks = vec![];
        for value in values {
            blocks.push(block_from_value(value)?);
        }
        Ok(blocks)
    }

    pub fn gettransaction(
        &self,
        txhash: &Sha256dHash,
        blockhash: Option<Sha256dHash>,
    ) -> Result<Transaction> {
        let mut args = json!([txhash.be_hex_string(), /*verbose=*/ false]);
        if let Some(blockhash) = blockhash {
            args.as_array_mut()
                .unwrap()
                .push(json!(blockhash.be_hex_string()));
        }
        tx_from_value(self.request("getrawtransaction", args)?)
    }

    pub fn gettransactions(&self, txhashes: &[&Sha256dHash]) -> Result<Vec<Transaction>> {
        let params_list: Vec<Value> = txhashes
            .iter()
            .map(|txhash| json!([txhash.be_hex_string(), /*verbose=*/ false]))
            .collect();

        let values = self.requests("getrawtransaction", &params_list)?;
        let mut txs = vec![];
        for value in values {
            txs.push(tx_from_value(value)?);
        }
        assert_eq!(txhashes.len(), txs.len());
        Ok(txs)
    }

    pub fn getmempooltxids(&self) -> Result<HashSet<Sha256dHash>> {
        let txids: Value = self.request("getrawmempool", json!([/*verbose=*/ false]))?;
        let mut result = HashSet::new();
        for value in txids.as_array().chain_err(|| "non-array result")? {
            result.insert(parse_hash(&value).chain_err(|| "invalid txid")?);
        }
        Ok(result)
    }

    pub fn getmempoolentry(&self, txid: &Sha256dHash) -> Result<MempoolEntry> {
        let entry = self.request("getmempoolentry", json!([txid.be_hex_string()]))?;
        let fee = (entry
            .get("fee")
            .chain_err(|| "missing fee")?
            .as_f64()
            .chain_err(|| "non-float fee")? * 100_000_000f64) as u64;
        let vsize = entry
            .get("size")
            .chain_err(|| "missing size")?
            .as_u64()
            .chain_err(|| "non-integer size")? as u32;
        Ok(MempoolEntry::new(fee, vsize))
    }

    pub fn broadcast(&self, tx: &Transaction) -> Result<Sha256dHash> {
        let tx = hex::encode(serialize(tx).unwrap());
        let txid = self.request("sendrawtransaction", json!([tx]))?;
        Ok(
            Sha256dHash::from_hex(txid.as_str().chain_err(|| "non-string txid")?)
                .chain_err(|| "failed to parse txid")?,
        )
    }

    fn get_all_headers(&self, tip: &Sha256dHash) -> Result<Vec<BlockHeader>> {
        let info: Value = self.request("getblockheader", json!([tip.be_hex_string()]))?;
        let tip_height = info.get("height")
            .expect("missing height")
            .as_u64()
            .expect("non-numeric height") as usize;
        let all_heights: Vec<usize> = (0..tip_height + 1).collect();
        let chunk_size = 100_000;
        let mut result = vec![];
        let null_hash = Sha256dHash::default();
        for heights in all_heights.chunks(chunk_size) {
            trace!("downloading {} block headers", heights.len());
            let mut headers = self.getblockheaders(&heights)?;
            assert!(headers.len() == heights.len());
            result.append(&mut headers);
        }

        let mut blockhash = null_hash;
        for header in &result {
            assert_eq!(header.prev_blockhash, blockhash);
            blockhash = header.bitcoin_hash();
        }
        assert_eq!(blockhash, *tip);
        Ok(result)
    }

    // Returns a list of BlockHeaders in ascending height (i.e. the tip is last).
    pub fn get_new_headers(
        &self,
        indexed_headers: &HeaderList,
        bestblockhash: &Sha256dHash,
    ) -> Result<Vec<BlockHeader>> {
        // Iterate back over headers until known blockash is found:
        if indexed_headers.len() == 0 {
            return self.get_all_headers(bestblockhash);
        }
        debug!(
            "downloading new block headers ({} already indexed) from {}",
            indexed_headers.len(),
            bestblockhash,
        );
        let mut new_headers = vec![];
        let null_hash = Sha256dHash::default();
        let mut blockhash = *bestblockhash;
        while blockhash != null_hash {
            if indexed_headers.header_by_blockhash(&blockhash).is_some() {
                break;
            }
            let header = self.getblockheader(&blockhash)
                .chain_err(|| format!("failed to get {} header", blockhash))?;
            new_headers.push(header);
            blockhash = header.prev_blockhash;
        }
        trace!("downloaded {} block headers", new_headers.len());
        new_headers.reverse(); // so the tip is the last vector entry
        Ok(new_headers)
    }
}
