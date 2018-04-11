use bitcoin::blockdata::block::BlockHeader;
use bitcoin::network::encodable::ConsensusDecodable;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::RawDecoder;
use itertools::enumerate;
use reqwest;
use std::collections::VecDeque;
use std::io::Cursor;

use util;

use {Bytes, HeaderMap};

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
        let mut headers = HeaderMap::new();
        let mut blockhash: Bytes = util::from_hex(
            "6fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d6190000000000",
        ).unwrap(); // genesis block hash
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

    pub fn enumerate_headers(&self) -> Vec<(usize, BlockHeader)> {
        let (mut header_map, mut blockhash) = self.get_headers();
        let mut header_list = VecDeque::<BlockHeader>::new();

        let null_hash = [0u8; 32];
        while blockhash != null_hash {
            let header: BlockHeader = header_map.remove(&blockhash).unwrap();
            blockhash = header.prev_blockhash[..].to_vec();
            header_list.push_front(header);
        }
        assert!(header_map.is_empty());
        enumerate(header_list).collect()
    }
}
