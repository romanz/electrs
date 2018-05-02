use bitcoin::blockdata::block::BlockHeader;
use bitcoin::network::encodable::ConsensusDecodable;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::deserialize;
use bitcoin::network::serialize::RawDecoder;
use reqwest;
use serde_json::{from_slice, Value};
use std::io::Cursor;

use index::HeaderList;
use types::{Bytes, HeaderMap, Sha256dHash};

error_chain!{}

const HEADER_SIZE: usize = 80;

pub struct Daemon {
    url: String,
}

impl Daemon {
    pub fn new(url: &str) -> Daemon {
        Daemon {
            url: url.to_string(),
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
        // Get current best blockhash (using REST API)
        let data = self.get("chaininfo.json")?;
        let reply: Value = from_slice(&data).chain_err(|| "failed to parse /chaininfo.json")?;
        let bestblockhash_hex = reply
            .get("bestblockhash")
            .chain_err(|| "missing bestblockhash")?
            .as_str()
            .chain_err(|| "non-string bestblockhash")?;
        let bestblockhash = Sha256dHash::from_hex(bestblockhash_hex).unwrap();
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
