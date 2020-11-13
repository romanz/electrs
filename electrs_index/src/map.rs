use anyhow::Result;
use bitcoin::{BlockHash, BlockHeader};
use chrono::{offset::TimeZone, Utc};

use std::{
    collections::{BTreeMap, HashMap},
    ops::Bound::{Included, Unbounded},
    time::Instant,
};

use crate::types::{BlockRow, FilePos};

#[derive(Default)]
struct Chain {
    by_height: Vec<BlockHash>,
    by_hash: HashMap<BlockHash, usize>,
}

impl Chain {
    fn build(tip: BlockHash, by_hash: &HashMap<BlockHash, HeaderPos>) -> Self {
        // verify full chain till genesis
        let mut by_height = vec![];
        let mut blockhash = tip;
        while blockhash != BlockHash::default() {
            by_height.push(blockhash);
            blockhash = match by_hash.get(&blockhash) {
                Some(value) => value.header.prev_blockhash,
                None => panic!("missing block header: {}", blockhash),
            };
        }
        by_height.reverse();
        let by_hash = by_height
            .iter()
            .enumerate()
            .map(|(index, blockhash)| (*blockhash, index))
            .collect();
        Self { by_height, by_hash }
    }

    fn len(&self) -> usize {
        self.by_height.len()
    }

    fn tip(&self) -> Option<&BlockHash> {
        self.by_height.last()
    }
}

struct HeaderPos {
    header: BlockHeader,
    pos: FilePos,
}

struct HashLimit {
    hash: BlockHash,
    limit: FilePos,
}

#[derive(Default)]
pub struct BlockMap {
    by_hash: HashMap<BlockHash, HeaderPos>,
    by_pos: BTreeMap<FilePos, HashLimit>, // start -> (limit, hash)
    chain: Chain,
}

impl BlockMap {
    pub(crate) fn new(block_rows: Vec<BlockRow>) -> Self {
        let mut map = Self::default();
        map.update_blocks(block_rows);
        map
    }

    pub fn chain(&self) -> &[BlockHash] {
        &self.chain.by_height
    }

    /// May return stale blocks
    pub fn get_by_hash(&self, hash: &BlockHash) -> Option<&BlockHeader> {
        self.by_hash.get(hash).map(|value| &value.header)
    }

    pub(crate) fn get_block_pos(&self, hash: &BlockHash) -> Option<&FilePos> {
        self.by_hash.get(hash).map(|value| &value.pos)
    }

    pub(crate) fn in_valid_chain(&self, hash: &BlockHash) -> bool {
        self.chain.by_hash.contains_key(hash)
    }

    fn update_blocks(&mut self, block_rows: Vec<BlockRow>) {
        let start = Instant::now();
        let total_blocks = block_rows.len();
        let mut new_blocks = 0usize;
        for row in block_rows {
            let hash = row.header.block_hash();
            self.by_hash.entry(hash).or_insert_with(|| {
                new_blocks += 1;
                HeaderPos {
                    header: row.header,
                    pos: row.pos,
                }
            });
            let offset = FilePos {
                file_id: row.pos.file_id,
                offset: row.pos.offset,
            };
            let limit = FilePos {
                file_id: row.pos.file_id,
                offset: row.pos.offset + row.size,
            };
            assert!(self
                .by_pos
                .insert(offset, HashLimit { limit, hash })
                .is_none());
        }
        debug!(
            "added {}/{} headers at {} ms",
            new_blocks,
            total_blocks,
            start.elapsed().as_millis()
        );
    }

    pub(crate) fn find_block(&self, tx_pos: FilePos) -> Option<(&BlockHash, usize)> {
        // look up the block that ends after this position
        let (start, HashLimit { limit, hash }) =
            match self.by_pos.range((Unbounded, Included(tx_pos))).next_back() {
                Some(item) => item,
                None => panic!("block not found: {:?}", tx_pos),
            };
        // make sure the position is in the block
        assert!(tx_pos < *limit);
        assert!(tx_pos >= *start);
        // make sure it's part of an active chain
        self.chain.by_hash.get(hash).map(|height| (hash, *height))
    }

    pub(crate) fn update_chain(
        &mut self,
        block_rows: Vec<BlockRow>,
        tip: BlockHash,
    ) -> Result<BlockHash> {
        self.update_blocks(block_rows);
        assert_eq!(self.by_hash.len(), self.by_pos.len());
        // make sure there is no overlap between blocks
        let mut last_limit = FilePos {
            file_id: 0,
            offset: 0,
        };
        for (start, HashLimit { limit, hash }) in &self.by_pos {
            assert!(
                start < limit,
                "invalid block {}: bad start={:?} limit={:?}",
                hash,
                start,
                limit
            );
            assert!(
                last_limit < *start,
                "invalid block {}: overlap found, start={:?} last_limit={:?}",
                hash,
                start,
                last_limit
            );
            last_limit = *limit;
        }
        let chain = Chain::build(tip, &self.by_hash);
        assert_eq!(chain.tip(), Some(&tip));
        let tip_time = match self.get_by_hash(&tip) {
            Some(header) => Utc.timestamp(header.time.into(), 0).to_rfc3339(),
            None => panic!("missing tip: {}", tip),
        };
        info!(
            "verified {} blocks, tip={} @ {}",
            chain.len(),
            tip,
            tip_time
        );
        self.chain = chain;
        Ok(tip)
    }
}
