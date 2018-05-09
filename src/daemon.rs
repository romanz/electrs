use base64;
use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::blockdata::transaction::Transaction;
use bitcoin::network::encodable::ConsensusDecodable;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::deserialize;
use bitcoin::network::serialize::RawDecoder;
use hex;
use reqwest;
use serde_json::{from_str, Value};
use std::env::home_dir;
use std::io::{BufRead, BufReader, Cursor, Write};
use std::net::TcpStream;

use index::HeaderList;
use types::{Bytes, HeaderMap, Sha256dHash};
use util::read_contents;

error_chain!{}

const HEADER_SIZE: usize = 80;

fn read_cookie() -> Vec<u8> {
    let mut path = home_dir().unwrap();
    path.push(".bitcoin");
    path.push("testnet3");
    path.push(".cookie");
    read_contents(&path).expect("failed to read cookie")
}

pub struct Daemon {
    url: String,
    cookie_b64: String,
}

impl Daemon {
    pub fn new(url: &str) -> Daemon {
        Daemon {
            url: url.to_string(),
            cookie_b64: base64::encode(&read_cookie()),
        }
    }

    // TODO: use error_chain for errors here.
    fn call_http(&self, resource: &str) -> Result<reqwest::Response> {
        let url = format!("{}/rest/{}", self.url, resource);
        Ok(reqwest::get(&url)
            .chain_err(|| format!("failed to get {}", url))?
            .error_for_status()
            .chain_err(|| "invalid status")?)
    }

    pub fn get(&self, resource: &str) -> Result<Bytes> {
        let mut buf = Bytes::new();
        let mut resp = self.call_http(resource)?;
        resp.copy_to(&mut buf)
            .chain_err(|| "failed to read response")?;
        Ok(buf)
    }

    fn call_jsonrpc(&self, request: &Value) -> Result<Value> {
        let mut conn = TcpStream::connect("127.0.0.1:18332").chain_err(|| "failed to connect")?;
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

    fn get_all_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        let genesis_blockhash = self.request("getblockhash", json!([0]))?;
        let mut blockhash = Sha256dHash::from_hex(
            genesis_blockhash.as_str().expect("non-string blockhash"),
        ).expect("non-hex blockhash");
        loop {
            let data = self.get(&format!("headers/2000/{}.bin", blockhash.be_hex_string()))
                .chain_err(|| "failed to get headers")?;
            assert!(!data.is_empty());
            let num_of_headers = data.len() / HEADER_SIZE;
            let mut decoder = RawDecoder::new(Cursor::new(data));
            for _ in 0..num_of_headers {
                let header: BlockHeader = ConsensusDecodable::consensus_decode(&mut decoder)
                    .chain_err(|| format!("failed to parse blockheader {}", blockhash))?;
                blockhash = header.bitcoin_hash();
                headers.insert(blockhash, header);
            }
            if num_of_headers == 1 {
                break;
            }
        }
        Ok(headers)
    }

    fn add_missing_headers(&self, mut header_map: HeaderMap) -> Result<(HeaderMap, Sha256dHash)> {
        // Get current best blockhash (using JSONRPC API)
        let bestblockhash = self.getbestblockhash()?;
        // Iterate back over headers until known blockash is found:
        let mut blockhash = bestblockhash;
        let nullhash = Sha256dHash::default();
        while !header_map.contains_key(&blockhash) && blockhash != nullhash {
            let header = self.getblockheader(&blockhash)
                .chain_err(|| "failed to get missing headers")?;
            header_map.insert(blockhash, header);
            blockhash = header.prev_blockhash;
        }
        Ok((header_map, bestblockhash))
    }

    pub fn enumerate_headers(&self, indexed_headers: &HeaderList) -> Result<HeaderList> {
        info!("loading headers");
        let header_map = if indexed_headers.headers().is_empty() {
            self.get_all_headers()
                .chain_err(|| "failed to download all headers")?
        } else {
            indexed_headers.as_map()
        };
        info!("loaded headers");
        let (header_map, blockhash) = self.add_missing_headers(header_map)
            .chain_err(|| "failed to add missing headers")?;
        info!("added missing headers");
        Ok(HeaderList::build(header_map, blockhash))
    }
}
