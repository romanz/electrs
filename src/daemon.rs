use bitcoin::blockdata::block::BlockHeader;
use bitcoin::network::encodable::ConsensusDecodable;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::RawDecoder;
use itertools::enumerate;
use reqwest;
use std::collections::{HashMap, VecDeque};
use std::io::Cursor;

use util;

use Bytes;

const HEADER_SIZE: usize = 80;

type HeaderMap = HashMap<Bytes, BlockHeader>;

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

    fn get_headers(&self) -> (HeaderMap, Bytes) {
        let mut headers = HashMap::new();
        let mut blockhash: Bytes = vec![
            111, 226, 140, 10, 182, 241, 179, 114, 193, 166, 162, 70, 174, 99, 247, 79, 147, 30,
            131, 101, 225, 90, 8, 156, 104, 214, 25, 0, 0, 0, 0, 0,
        ]; // genesis block hash
        loop {
            let data = self.get(&format!("headers/2000/{}.bin", util::revhex(&blockhash)));
            assert!(!data.is_empty());
            let num_of_headers = data.len() / HEADER_SIZE;
            let mut decoder = RawDecoder::new(Cursor::new(data));
            for _ in 0..num_of_headers {
                let header: BlockHeader =
                    ConsensusDecodable::consensus_decode(&mut decoder).unwrap();
                blockhash = header.bitcoin_hash()[..].to_vec();
                headers.insert(blockhash.clone(), header);
            }
            if num_of_headers == 1 {
                break;
            }
        }
        (headers, blockhash)
    }

    pub fn enumerate_headers(&self) -> Vec<(usize, Bytes)> {
        let (headers, mut blockhash) = self.get_headers();
        let mut hashes = VecDeque::<Bytes>::new();

        let null_hash = [0u8; 32];
        while blockhash != null_hash {
            let header: &BlockHeader = headers.get(&blockhash).unwrap();
            hashes.push_front(blockhash);
            blockhash = header.prev_blockhash[..].to_vec();
        }
        enumerate(hashes).collect()
    }
}
