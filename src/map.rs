use bitcoin::{BlockHash, BlockHeader};

use std::collections::HashMap;

#[derive(Default)]
struct Chain {
    by_height: Vec<BlockHash>,
}

impl Chain {
    fn build(tip: BlockHash, by_hash: &HashMap<BlockHash, BlockHeader>) -> Self {
        // verify full chain till genesis
        let mut by_height = vec![];
        let mut blockhash = tip;
        while blockhash != BlockHash::default() {
            by_height.push(blockhash);
            blockhash = match by_hash.get(&blockhash) {
                Some(header) => header.prev_blockhash,
                None => panic!("missing block header: {}", blockhash),
            };
        }
        by_height.reverse();
        Self { by_height }
    }

    fn len(&self) -> usize {
        self.by_height.len()
    }

    fn tip(&self) -> Option<&BlockHash> {
        self.by_height.last()
    }
}

#[derive(Default)]
pub struct BlockMap {
    by_hash: HashMap<BlockHash, BlockHeader>,
    chain: Chain,
}

impl BlockMap {
    pub(crate) fn new(headers: Vec<BlockHeader>) -> Self {
        let mut map = Self::default();
        map.add_headers(headers);
        map
    }

    pub fn chain(&self) -> &[BlockHash] {
        &self.chain.by_height
    }

    /// May return stale headers
    pub fn get_header(&self, hash: &BlockHash) -> Option<&BlockHeader> {
        self.by_hash.get(hash)
    }

    fn add_headers(&mut self, headers: Vec<BlockHeader>) {
        let total_blocks = headers.len();
        let mut new_blocks = 0usize;
        for header in headers {
            let hash = header.block_hash();
            self.by_hash.entry(hash).or_insert_with(|| {
                new_blocks += 1;
                header
            });
        }
        debug!("added {}/{} headers", new_blocks, total_blocks,);
    }

    pub fn update(&mut self, tip: BlockHash, headers: Vec<BlockHeader>) {
        self.add_headers(headers);
        let chain = Chain::build(tip, &self.by_hash);
        assert_eq!(chain.tip(), Some(&tip));
        info!("verified {} headers, tip={}", chain.len(), tip);
        self.chain = chain;
    }
}
