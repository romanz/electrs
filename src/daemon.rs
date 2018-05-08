use base64;
use bitcoin::blockdata::block::BlockHeader;
use bitcoin::network::encodable::ConsensusDecodable;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::deserialize;
use bitcoin::network::serialize::RawDecoder;
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
    path.push(".cookie");
    read_contents(&path).unwrap()
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
    fn request(&self, resource: &str) -> Result<reqwest::Response> {
        let url = format!("{}/rest/{}", self.url, resource);
        Ok(reqwest::get(&url)
            .chain_err(|| format!("failed to get {}", url))?
            .error_for_status()
            .chain_err(|| "invalid status")?)
    }

    pub fn get(&self, resource: &str) -> Result<Bytes> {
        let mut buf = Bytes::new();
        let mut resp = self.request(resource)?;
        resp.copy_to(&mut buf)
            .chain_err(|| "failed to read response")?;
        Ok(buf)
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let mut conn = TcpStream::connect("127.0.0.1:8332").chain_err(|| "failed to connect")?;
        let request = json!({"method": method, "params": params}).to_string();
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
        let mut reply: Value = from_str(&contents).chain_err(|| "invalid JSON")?;
        let err = reply["error"].take();
        if !err.is_null() {
            bail!("called failed: {}", err);
        }
        Ok(reply["result"].take())
    }

    fn get_all_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        let mut blockhash = Sha256dHash::from_hex(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        ).unwrap(); // genesis block hash
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
        let reply = self.call("getbestblockhash", json!([]))?;
        let bestblockhash =
            Sha256dHash::from_hex(reply.as_str().chain_err(|| "non-string bestblockhash")?)
                .chain_err(|| "non-hex bestblockhash")?;
        // Iterate back over headers until known blockash is found:
        let mut blockhash = bestblockhash;
        while !header_map.contains_key(&blockhash) {
            let data = self.get(&format!("headers/1/{}.bin", blockhash.be_hex_string()))?;
            let header: BlockHeader = deserialize(&data)
                .chain_err(|| format!("failed to parse blockheader {}", blockhash))?;
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
