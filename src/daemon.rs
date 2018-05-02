use bitcoin::blockdata::block::BlockHeader;
use bitcoin::network::encodable::ConsensusDecodable;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::deserialize;
use bitcoin::network::serialize::RawDecoder;
use reqwest;
use serde_json::{from_slice, Value};
use std::collections::VecDeque;
use std::io::Cursor;
use std::iter::FromIterator;

use types::{Bytes, HeaderMap, Sha256dHash};

const HEADER_SIZE: usize = 80;

pub struct Daemon {
    url: String,
}

#[derive(Eq, PartialEq, Clone)]
pub struct HeaderEntry {
    height: usize,
    hash: Sha256dHash,
    header: BlockHeader,
}

impl HeaderEntry {
    pub fn hash(&self) -> &Sha256dHash {
        &self.hash
    }

    pub fn header(&self) -> &BlockHeader {
        &self.header
    }

    pub fn height(&self) -> usize {
        self.height
    }
}

pub struct HeaderList {
    headers: Vec<HeaderEntry>,
}

impl HeaderList {
    pub fn empty() -> HeaderList {
        HeaderList { headers: vec![] }
    }

    pub fn equals(&self, other: &HeaderList) -> bool {
        self.headers.last() == other.headers.last()
    }

    pub fn headers(&self) -> &[HeaderEntry] {
        &self.headers
    }

    pub fn as_map(&self) -> HeaderMap {
        HeaderMap::from_iter(self.headers.iter().map(|entry| (entry.hash, entry.header)))
    }
}

impl Daemon {
    pub fn new(url: &str) -> Daemon {
        Daemon {
            url: url.to_string(),
        }
    }

    // TODO: use error_chain for errors here.
    fn request(&self, resource: &str) -> reqwest::Response {
        let url = format!("{}/rest/{}", self.url, resource);
        reqwest::get(&url).unwrap().error_for_status().unwrap()
    }

    pub fn get(&self, resource: &str) -> Bytes {
        let mut buf = Bytes::new();
        let mut resp = self.request(resource);
        resp.copy_to(&mut buf).unwrap();
        buf
    }

    fn get_all_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let mut blockhash = Sha256dHash::from_hex(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        ).unwrap(); // genesis block hash
        loop {
            let data = self.get(&format!("headers/2000/{}.bin", blockhash.be_hex_string()));
            assert!(!data.is_empty());
            let num_of_headers = data.len() / HEADER_SIZE;
            let mut decoder = RawDecoder::new(Cursor::new(data));
            for _ in 0..num_of_headers {
                let header: BlockHeader =
                    ConsensusDecodable::consensus_decode(&mut decoder).unwrap();
                blockhash = header.bitcoin_hash();
                headers.insert(blockhash, header);
            }
            if num_of_headers == 1 {
                break;
            }
        }
        headers
    }

    fn add_missing_headers(&self, header_map: &mut HeaderMap) -> Sha256dHash {
        // Get current best blockhash (using REST API)
        let data = self.get("chaininfo.json");
        let reply: Value = from_slice(&data).unwrap();
        let bestblockhash_hex = reply.get("bestblockhash").unwrap().as_str().unwrap();
        let bestblockhash = Sha256dHash::from_hex(bestblockhash_hex).unwrap();
        // Iterate back over headers until known blockash is found:
        let mut blockhash = bestblockhash;
        while !header_map.contains_key(&blockhash) {
            let data = self.get(&format!("headers/1/{}.bin", blockhash.be_hex_string()));
            let header: BlockHeader = deserialize(&data).unwrap();
            header_map.insert(blockhash, header);
            blockhash = header.prev_blockhash;
        }
        bestblockhash
    }

    pub fn enumerate_headers(&self, indexed_headers: &HeaderList) -> HeaderList {
        let mut header_map = if indexed_headers.headers.is_empty() {
            self.get_all_headers()
        } else {
            indexed_headers.as_map()
        };
        let mut blockhash = self.add_missing_headers(&mut header_map);

        let null_hash = Sha256dHash::default();

        struct HashedHeader {
            blockhash: Sha256dHash,
            header: BlockHeader,
        }
        let mut hashed_headers = VecDeque::<HashedHeader>::new();
        while blockhash != null_hash {
            let header: BlockHeader = header_map.remove(&blockhash).unwrap();
            hashed_headers.push_front(HashedHeader { blockhash, header });
            blockhash = header.prev_blockhash;
        }
        assert!(header_map.is_empty());
        HeaderList {
            headers: hashed_headers
                .into_iter()
                .enumerate()
                .map(|(height, hashed_header)| HeaderEntry {
                    height: height,
                    hash: hashed_header.blockhash,
                    header: hashed_header.header,
                })
                .collect(),
        }
    }
}
