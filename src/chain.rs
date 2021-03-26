use std::collections::HashMap;

use bitcoin::consensus::deserialize;
use bitcoin::hashes::hex::FromHex;
use bitcoin::network::constants;
use bitcoin::{BlockHash, BlockHeader};

pub(crate) struct NewHeader {
    header: BlockHeader,
    hash: BlockHash,
    height: usize,
}

impl NewHeader {
    pub(crate) fn from((header, height): (BlockHeader, usize)) -> Self {
        Self {
            header,
            hash: header.block_hash(),
            height,
        }
    }

    pub(crate) fn height(&self) -> usize {
        self.height
    }

    pub(crate) fn hash(&self) -> BlockHash {
        self.hash
    }
}

/// Curent blockchain headers' list
pub struct Chain {
    headers: Vec<(BlockHash, BlockHeader)>,
    heights: HashMap<BlockHash, usize>,
}

impl Chain {
    pub fn new(network: constants::Network) -> Self {
        let genesis_header_hex = match network {
            constants::Network::Bitcoin => "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c",
            constants::Network::Testnet => "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4adae5494dffff001d1aa4ae18",
            constants::Network::Regtest => "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4adae5494dffff7f2002000000",
            constants::Network::Signet => "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a008f4d5fae77031e8ad22203",
        };
        let genesis_header_bytes = Vec::from_hex(genesis_header_hex).unwrap();
        let genesis: BlockHeader = deserialize(&genesis_header_bytes).unwrap();
        assert_eq!(genesis.prev_blockhash, BlockHash::default());
        Self {
            headers: vec![(genesis.block_hash(), genesis)],
            heights: std::iter::once((genesis.block_hash(), 0)).collect(),
        }
    }

    pub(crate) fn load(&mut self, headers: Vec<BlockHeader>, tip: BlockHash) {
        let genesis_hash = self.headers[0].0;

        let mut header_map: HashMap<BlockHash, BlockHeader> =
            headers.into_iter().map(|h| (h.block_hash(), h)).collect();
        let mut blockhash = tip;
        let mut new_headers = vec![];
        while blockhash != genesis_hash {
            let header = match header_map.remove(&blockhash) {
                Some(header) => header,
                None => panic!("missing header {} while loading from DB", blockhash),
            };
            blockhash = header.prev_blockhash;
            new_headers.push(header);
        }
        info!("loading {} headers, tip={}", new_headers.len(), tip);
        let new_headers = new_headers.into_iter().rev(); // order by height
        self.update(new_headers.zip(1..).map(NewHeader::from).collect())
    }

    pub(crate) fn get_block_hash(&self, height: usize) -> Option<BlockHash> {
        self.headers.get(height).map(|(hash, _header)| *hash)
    }

    pub(crate) fn get_block_header(&self, height: usize) -> Option<&BlockHeader> {
        self.headers.get(height).map(|(_hash, header)| header)
    }

    pub(crate) fn get_block_height(&self, blockhash: &BlockHash) -> Option<usize> {
        self.heights.get(blockhash).copied()
    }

    pub(crate) fn update(&mut self, headers: Vec<NewHeader>) {
        if let Some(first_height) = headers.first().map(|h| h.height) {
            for (hash, _header) in self.headers.drain(first_height..) {
                assert!(self.heights.remove(&hash).is_some());
            }
            for (h, height) in headers.into_iter().zip(first_height..) {
                assert_eq!(h.height, height);
                assert_eq!(h.hash, h.header.block_hash());
                assert!(self.heights.insert(h.hash, h.height).is_none());
                self.headers.push((h.hash, h.header));
            }
            info!(
                "chain updated: tip={}, height={}",
                self.headers.last().unwrap().0,
                self.headers.len() - 1
            );
        }
    }

    pub(crate) fn tip(&self) -> BlockHash {
        self.headers.last().expect("empty chain").0
    }

    pub(crate) fn height(&self) -> usize {
        self.headers.len() - 1
    }

    pub(crate) fn locator(&self) -> Vec<BlockHash> {
        let mut result = vec![];
        let mut index = self.headers.len() - 1;
        let mut step = 1;
        loop {
            if result.len() >= 10 {
                step *= 2;
            }
            result.push(self.headers[index].0);
            if index == 0 {
                break;
            }
            index = index.saturating_sub(step);
        }
        result
    }
}
