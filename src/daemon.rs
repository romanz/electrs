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
use std::str::FromStr;
use std::sync::Mutex;

use metrics::{HistogramOpts, HistogramVec, Metrics};
use util::HeaderList;

use errors::*;

#[derive(Debug, Copy, Clone)]
pub enum Network {
    Mainnet,
    Testnet,
}

fn parse_hash(value: &Value) -> Result<Sha256dHash> {
    Ok(
        Sha256dHash::from_hex(value.as_str().chain_err(|| "non-string value")?)
            .chain_err(|| "non-hex value")?,
    )
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

fn parse_jsonrpc_reply(reply: &mut Value, method: &str) -> Result<Value> {
    let reply_obj = reply.as_object_mut().chain_err(|| "non-object reply")?;
    if let Some(err) = reply_obj.get("error") {
        if !err.is_null() {
            bail!("{} RPC error: {}", method, err);
        }
    }
    let result = reply_obj
        .get_mut("result")
        .chain_err(|| "no result in reply")?;
    Ok(result.take())
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

struct Connection {
    tx: TcpStream,
    rx: Lines<BufReader<TcpStream>>,
    cookie_b64: String,
    addr: SocketAddr,
}

impl Connection {
    fn new(addr: SocketAddr, cookie_b64: String) -> Result<Connection> {
        let conn = TcpStream::connect(addr).chain_err(|| format!("failed to connect to {}", addr))?;
        let reader = BufReader::new(conn.try_clone()
            .chain_err(|| format!("failed to clone {:?}", conn))?);
        Ok(Connection {
            tx: conn,
            rx: reader.lines(),
            cookie_b64,
            addr,
        })
    }

    pub fn reconnect(&self) -> Result<Connection> {
        let conn = TcpStream::connect(self.addr)
            .chain_err(|| format!("failed to connect to {}", self.addr))?;
        let reader = BufReader::new(conn.try_clone()
            .chain_err(|| format!("failed to clone {:?}", conn))?);
        Ok(Connection {
            tx: conn,
            rx: reader.lines(),
            cookie_b64: self.cookie_b64.clone(),
            addr: self.addr,
        })
    }

    fn send(&mut self, request: &str) -> Result<()> {
        let msg = format!(
            "POST / HTTP/1.1\nAuthorization: Basic {}\nContent-Length: {}\n\n{}",
            self.cookie_b64,
            request.len(),
            request,
        );
        self.tx
            .write_all(msg.as_bytes())
            .chain_err(|| "failed to send request")
    }

    fn recv(&mut self) -> Result<String> {
        // TODO: use proper HTTP parser.
        let mut in_header = true;
        let mut contents: Option<String> = None;
        let iter = self.rx.by_ref();
        let status = iter.next()
            .chain_err(|| "disconnected from daemon")?
            .chain_err(|| "failed to read status")?;
        if status != "HTTP/1.1 200 OK" {
            bail!("request failed: {}", status);
        }
        for line in iter {
            let line = line.chain_err(|| "failed to read")?;
            if line.is_empty() {
                in_header = false; // next line should contain the actual response.
            } else if !in_header {
                contents = Some(line);
                break;
            }
        }
        contents.chain_err(|| "no reply")
    }
}

pub struct Daemon {
    daemon_dir: PathBuf,
    network: Network,
    conn: Mutex<Connection>,

    // monitoring
    latency: HistogramVec,
    size: HistogramVec,
}

impl Daemon {
    pub fn new(
        daemon_dir: &PathBuf,
        cookie: &str,
        network: Network,
        metrics: &Metrics,
    ) -> Result<Daemon> {
        let addr = match network {
            Network::Mainnet => "127.0.0.1:8332",
            Network::Testnet => "127.0.0.1:18332",
        };
        let daemon = Daemon {
            daemon_dir: daemon_dir.clone(),
            network,
            conn: Mutex::new(Connection::new(
                SocketAddr::from_str(addr).unwrap(),
                base64::encode(cookie),
            )?),
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
            latency: self.latency.clone(),
            size: self.size.clone(),
        })
    }

    pub fn list_blk_files(&self) -> Result<Vec<PathBuf>> {
        let mut path = self.daemon_dir.clone();
        path.push("blocks");
        path.push("blk*.dat");
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
        }
    }

    fn call_jsonrpc(&self, method: &str, request: &Value) -> Result<Value> {
        let timer = self.latency.with_label_values(&[method]).start_timer();
        let mut conn = self.conn.lock().unwrap();
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

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let req = json!({"method": method, "params": params});
        let mut reply = self.call_jsonrpc(method, &req)
            .chain_err(|| format!("RPC failed: {}", req))?;
        parse_jsonrpc_reply(&mut reply, method)
    }

    fn requests(&self, method: &str, params_list: &[Value]) -> Result<Vec<Value>> {
        let reqs = params_list
            .iter()
            .map(|params| json!({"method": method, "params": params}))
            .collect();
        let mut results = vec![];
        let mut replies = self.call_jsonrpc(method, &reqs)
            .chain_err(|| format!("RPC failed: {}", reqs))?;
        for reply in replies.as_array_mut().chain_err(|| "non-array response")? {
            results.push(parse_jsonrpc_reply(reply, method)?)
        }
        Ok(results)
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

    pub fn gettransactions(&self, txhashes: &[Sha256dHash]) -> Result<Vec<Transaction>> {
        let params_list: Vec<Value> = txhashes
            .iter()
            .map(|txhash| json!([txhash.be_hex_string(), /*verbose=*/ false]))
            .collect();

        let values = self.requests("getrawtransaction", &params_list)?;
        let mut txs = vec![];
        for value in values {
            txs.push(tx_from_value(value)?);
        }
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
        let fees = entry
            .get("fees")
            .chain_err(|| "missing fees section")?
            .as_object()
            .chain_err(|| "non-object fees")?;
        let fee = (fees.get("base")
            .chain_err(|| "missing base fee")?
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
            trace!("downloaded {} block header", blockhash);
            new_headers.push(header);
            blockhash = header.prev_blockhash;
        }
        new_headers.reverse(); // so the tip is the last vector entry
        Ok(new_headers)
    }
}
