use base64;
use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::blockdata::transaction::Transaction;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::deserialize;
use hex;
use serde_json::{from_str, Value};
use std::env::home_dir;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

use index::HeaderList;
use types::{HeaderMap, Sha256dHash};
use util::read_contents;

error_chain!{}

fn read_cookie() -> Vec<u8> {
    let mut path = home_dir().unwrap();
    path.push(".bitcoin");
    path.push(".cookie");
    read_contents(&path).expect("failed to read cookie")
}

pub struct Daemon {
    addr: String,
    cookie_b64: String,
}

impl Daemon {
    pub fn new(addr: &str) -> Daemon {
        Daemon {
            addr: addr.to_string(),
            cookie_b64: base64::encode(&read_cookie()),
        }
    }

    fn call_jsonrpc(&self, request: &Value) -> Result<Value> {
        let mut conn = TcpStream::connect(&self.addr)
            .chain_err(|| format!("failed to connect to {}", self.addr))?;
        let request = request.to_string();
        let msg = format!(
            "POST / HTTP/1.1\nAuthorization: Basic {}\nContent-Length: {}\n\n{}",
            self.cookie_b64,
            request.len(),
            request,
        );
        conn.write_all(msg.as_bytes())
            .chain_err(|| "failed to send request")?;

        let mut in_header = true;
        let mut contents: Option<String> = None;
        for line in BufReader::new(conn).lines() {
            let line = line.chain_err(|| "failed to read")?;
            if line.is_empty() {
                in_header = false;
            } else if !in_header {
                contents = Some(line);
                break;
            }
        }
        let contents = contents.chain_err(|| "no reply")?;
        let reply: Value = from_str(&contents).chain_err(|| "invalid JSON")?;
        Ok(reply)
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let req = json!({"method": method, "params": params});
        let mut reply = self.call_jsonrpc(&req)
            .chain_err(|| format!("RPC failed: {}", req))?;
        let err = reply["error"].take();
        if !err.is_null() {
            bail!("{} RPC error: {}", method, err);
        }
        Ok(reply["result"].take())
    }

    fn requests(&self, method: &str, params_list: &[Value]) -> Result<Vec<Value>> {
        let reqs = params_list
            .iter()
            .map(|params| json!({"method": method, "params": params}))
            .collect();
        let mut result = Vec::new();
        for reply in self.call_jsonrpc(&reqs)
            .chain_err(|| format!("RPC failed: {}", reqs))?
            .as_array_mut()
            .chain_err(|| "non-array response")?
        {
            let err = reply["error"].take();
            if !err.is_null() {
                bail!("{} RPC error: {}", method, err);
            }
            result.push(reply["result"].take())
        }
        Ok(result)
    }

    // bitcoind JSONRPC API:

    pub fn getbestblockhash(&self) -> Result<Sha256dHash> {
        let reply = self.request("getbestblockhash", json!([]))?;
        Ok(
            Sha256dHash::from_hex(reply.as_str().chain_err(|| "non-string bestblockhash")?)
                .chain_err(|| "non-hex bestblockhash")?,
        )
    }

    pub fn getblockheaders(&self, heights: &[usize]) -> Result<Vec<BlockHeader>> {
        let heights: Vec<Value> = heights.iter().map(|height| json!([height])).collect();
        let hashes: Vec<Value> = self.requests("getblockhash", &heights)?
            .into_iter()
            .map(|hash| json!([hash, /*verbose=*/ false]))
            .collect();
        let headers: Vec<Value> = self.requests("getblockheader", &hashes)?;

        fn header_from_value(value: Value) -> Result<BlockHeader> {
            let header_hex = value
                .as_str()
                .chain_err(|| format!("non-string header: {}", value))?;
            let header_bytes = hex::decode(header_hex).chain_err(|| "non-hex header")?;
            Ok(deserialize(&header_bytes)
                .chain_err(|| format!("failed to parse blockheader {}", header_hex))?)
        }
        let mut result = Vec::new();
        for h in headers {
            result.push(header_from_value(h)?);
        }
        Ok(result)
    }

    pub fn getblockheader(&self, blockhash: &Sha256dHash) -> Result<BlockHeader> {
        let header_hex: Value = self.request(
            "getblockheader",
            json!([blockhash.be_hex_string(), /*verbose=*/ false]),
        )?;
        Ok(deserialize(
            &hex::decode(header_hex.as_str().chain_err(|| "non-string header")?)
                .chain_err(|| "non-hex header")?,
        ).chain_err(|| format!("failed to parse blockheader {}", blockhash))?)
    }

    pub fn getblock(&self, blockhash: &Sha256dHash) -> Result<Block> {
        let block_hex: Value = self.request(
            "getblock",
            json!([blockhash.be_hex_string(), /*verbose=*/ false]),
        )?;
        Ok(deserialize(
            &hex::decode(block_hex.as_str().chain_err(|| "non-string block")?)
                .chain_err(|| "non-hex block")?,
        ).chain_err(|| format!("failed to parse block {}", blockhash))?)
    }

    pub fn gettransaction(&self, txhash: &Sha256dHash) -> Result<Transaction> {
        let tx_hex: Value = self.request(
            "getrawtransaction",
            json!([txhash.be_hex_string(), /*verbose=*/ false]),
        )?;
        Ok(
            deserialize(&hex::decode(tx_hex.as_str().chain_err(|| "non-string tx")?)
                .chain_err(|| "non-hex tx")?)
                .chain_err(|| format!("failed to parse tx {}", txhash))?,
        )
    }

    pub fn get_all_headers(&self) -> Result<HeaderMap> {
        let info: Value = self.request("getblockchaininfo", json!([]))?;
        let max_height = info.get("blocks")
            .expect("missing `blocks` attribute")
            .as_u64()
            .expect("`blocks` should be number") as usize;
        let all_heights: Vec<usize> = (0..max_height).collect();
        let chunk_size = 10_000;
        let mut result = HeaderMap::new();

        let null_hash = Sha256dHash::default();
        let mut blockhash = null_hash;
        for heights in all_heights.chunks(chunk_size) {
            let headers = self.getblockheaders(&heights)?;
            assert!(headers.len() == heights.len());
            for header in headers {
                blockhash = header.bitcoin_hash();
                result.insert(blockhash, header);
            }
        }

        while blockhash != null_hash {
            blockhash = result
                .get(&blockhash)
                .chain_err(|| format!("missing block header {}", blockhash))?
                .prev_blockhash;
        }
        Ok(result)
    }

    fn add_missing_headers(&self, mut header_map: HeaderMap) -> Result<(HeaderMap, Sha256dHash)> {
        // Get current best blockhash (using JSONRPC API)
        let bestblockhash = self.getbestblockhash()?;
        // Iterate back over headers until known blockash is found:
        let mut blockhash = bestblockhash;
        while !header_map.contains_key(&blockhash) {
            let header = self.getblockheader(&blockhash)
                .chain_err(|| "failed to get missing headers")?;
            header_map.insert(blockhash, header);
            blockhash = header.prev_blockhash;
        }
        Ok((header_map, bestblockhash))
    }

    pub fn enumerate_headers(&self, indexed_headers: &HeaderList) -> Result<HeaderList> {
        let header_map = if indexed_headers.headers().is_empty() {
            self.get_all_headers()
                .chain_err(|| "failed to download all headers")?
        } else {
            indexed_headers.as_map()
        };
        let (header_map, blockhash) = self.add_missing_headers(header_map)
            .chain_err(|| "failed to add missing headers")?;
        Ok(HeaderList::build(header_map, blockhash))
    }
}
