use bitcoin::blockdata::block::BlockHeader;
use bitcoin::network::encodable::ConsensusDecodable;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::RawDecoder;
use bitcoin::util::hash::Sha256dHash;
use itertools::enumerate;
use reqwest;
use std::collections::{HashMap, VecDeque};
use std::io::Cursor;

use Bytes;

const HEADER_SIZE: usize = 80;

type HeaderMap = HashMap<String, BlockHeader>;

pub struct Daemon {
    url: String,
}

impl Daemon {
    pub fn new(url: &str) -> Daemon {
        Daemon {
            url: url.to_string(),
        }
    }

    fn request(&self, resource: &str) -> reqwest::Response {
        let url = format!("{}/rest/{}", self.url, resource);
        reqwest::get(&url).unwrap()
    }

    pub fn get(&self, resource: &str) -> Bytes {
        let mut buf = Bytes::new();
        let mut resp = self.request(resource);
        resp.copy_to(&mut buf).unwrap();
        buf
    }

    fn get_headers(&self) -> (HeaderMap, String) {
        let mut headers = HashMap::new();
        let mut blockhash =
            String::from("000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"); // genesis
        loop {
            let data = self.get(&format!("headers/2000/{}.bin", blockhash));
            let num_of_headers = data.len() / HEADER_SIZE;
            let mut decoder = RawDecoder::new(Cursor::new(data));
            for _ in 0..num_of_headers {
                let header: BlockHeader =
                    ConsensusDecodable::consensus_decode(&mut decoder).unwrap();
                blockhash = header.bitcoin_hash().be_hex_string();
                headers.insert(blockhash.to_string(), header);
            }
            if num_of_headers == 1 {
                break;
            }
        }
        (headers, blockhash)
    }

    pub fn enumerate_headers(&self) -> Vec<(usize, String)> {
        let (headers, mut blockhash) = self.get_headers();
        let mut hashes = VecDeque::<String>::new();

        let null_hash = Sha256dHash::default().be_hex_string();
        while blockhash != null_hash {
            let header: &BlockHeader = headers.get(&blockhash).unwrap();
            hashes.push_front(blockhash);
            blockhash = header.prev_blockhash.be_hex_string();
        }
        enumerate(hashes).collect()
    }
}
