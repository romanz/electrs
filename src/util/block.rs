use crate::chain::{BlockHash, BlockHeader};
use crate::errors::*;
use crate::new_index::BlockEntry;

use std::collections::HashMap;
use std::fmt;
use std::iter::FromIterator;
use std::slice;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime as DateTime;

const MTP_SPAN: usize = 11;

lazy_static! {
    pub static ref DEFAULT_BLOCKHASH: BlockHash =
        "0000000000000000000000000000000000000000000000000000000000000000"
            .parse()
            .unwrap();
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BlockId {
    pub height: usize,
    pub hash: BlockHash,
    pub time: u32,
}

impl From<&HeaderEntry> for BlockId {
    fn from(header: &HeaderEntry) -> Self {
        BlockId {
            height: header.height(),
            hash: *header.hash(),
            time: header.header().time,
        }
    }
}

#[derive(Eq, PartialEq, Clone)]
pub struct HeaderEntry {
    height: usize,
    hash: BlockHash,
    header: BlockHeader,
}

impl HeaderEntry {
    pub fn hash(&self) -> &BlockHash {
        &self.hash
    }

    pub fn header(&self) -> &BlockHeader {
        &self.header
    }

    pub fn height(&self) -> usize {
        self.height
    }
}

impl fmt::Debug for HeaderEntry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let last_block_time = DateTime::from_unix_timestamp(self.header().time as i64).unwrap();
        write!(
            f,
            "hash={} height={} @ {}",
            self.hash(),
            self.height(),
            last_block_time.format(&Rfc3339).unwrap(),
        )
    }
}

pub struct HeaderList {
    headers: Vec<HeaderEntry>,
    heights: HashMap<BlockHash, usize>,
    tip: BlockHash,
}

impl HeaderList {
    pub fn empty() -> HeaderList {
        HeaderList {
            headers: vec![],
            heights: HashMap::new(),
            tip: *DEFAULT_BLOCKHASH,
        }
    }

    pub fn new(
        mut headers_map: HashMap<BlockHash, BlockHeader>,
        tip_hash: BlockHash,
    ) -> HeaderList {
        trace!(
            "processing {} headers, tip at {:?}",
            headers_map.len(),
            tip_hash
        );

        let mut blockhash = tip_hash;
        let mut headers_chain: Vec<BlockHeader> = vec![];

        while blockhash != *DEFAULT_BLOCKHASH {
            let header = headers_map.remove(&blockhash).unwrap_or_else(|| {
                panic!(
                    "missing expected blockhash in headers map: {:?}, pointed from: {:?}",
                    blockhash,
                    headers_chain.last().map(|h| h.block_hash())
                )
            });
            blockhash = header.prev_blockhash;
            headers_chain.push(header);
        }
        headers_chain.reverse();

        trace!(
            "{} chained headers ({} orphan blocks left)",
            headers_chain.len(),
            headers_map.len()
        );

        let mut headers = HeaderList::empty();
        headers.apply(headers.order(headers_chain));
        headers
    }

    pub fn order(&self, new_headers: Vec<BlockHeader>) -> Vec<HeaderEntry> {
        // header[i] -> header[i-1] (i.e. header.last() is the tip)
        struct HashedHeader {
            blockhash: BlockHash,
            header: BlockHeader,
        }
        let hashed_headers =
            Vec::<HashedHeader>::from_iter(new_headers.into_iter().map(|header| HashedHeader {
                blockhash: header.block_hash(),
                header,
            }));
        for i in 1..hashed_headers.len() {
            assert_eq!(
                hashed_headers[i].header.prev_blockhash,
                hashed_headers[i - 1].blockhash
            );
        }
        let prev_blockhash = match hashed_headers.first() {
            Some(h) => h.header.prev_blockhash,
            None => return vec![], // hashed_headers is empty
        };
        let new_height: usize = if prev_blockhash == *DEFAULT_BLOCKHASH {
            0
        } else {
            self.header_by_blockhash(&prev_blockhash)
                .unwrap_or_else(|| panic!("{} is not part of the blockchain", prev_blockhash))
                .height()
                + 1
        };
        (new_height..)
            .zip(hashed_headers.into_iter())
            .map(|(height, hashed_header)| HeaderEntry {
                height,
                hash: hashed_header.blockhash,
                header: hashed_header.header,
            })
            .collect()
    }

    pub fn apply(&mut self, new_headers: Vec<HeaderEntry>) {
        // new_headers[i] -> new_headers[i - 1] (i.e. new_headers.last() is the tip)
        for i in 1..new_headers.len() {
            assert_eq!(new_headers[i - 1].height() + 1, new_headers[i].height());
            assert_eq!(
                *new_headers[i - 1].hash(),
                new_headers[i].header().prev_blockhash
            );
        }
        let new_height = match new_headers.first() {
            Some(entry) => {
                let height = entry.height();
                let expected_prev_blockhash = if height > 0 {
                    *self.headers[height - 1].hash()
                } else {
                    *DEFAULT_BLOCKHASH
                };
                assert_eq!(entry.header().prev_blockhash, expected_prev_blockhash);
                height
            }
            None => return,
        };
        debug!(
            "applying {} new headers from height {}",
            new_headers.len(),
            new_height
        );
        let _removed = self.headers.split_off(new_height); // keep [0..new_height) entries
        for new_header in new_headers {
            let height = new_header.height();
            assert_eq!(height, self.headers.len());
            self.tip = *new_header.hash();
            self.headers.push(new_header);
            self.heights.insert(self.tip, height);
        }
    }

    pub fn header_by_blockhash(&self, blockhash: &BlockHash) -> Option<&HeaderEntry> {
        let height = self.heights.get(blockhash)?;
        let header = self.headers.get(*height)?;
        if *blockhash == *header.hash() {
            Some(header)
        } else {
            None
        }
    }

    pub fn header_by_height(&self, height: usize) -> Option<&HeaderEntry> {
        self.headers.get(height).map(|entry| {
            assert_eq!(entry.height(), height);
            entry
        })
    }

    pub fn equals(&self, other: &HeaderList) -> bool {
        self.headers.last() == other.headers.last()
    }

    pub fn tip(&self) -> &BlockHash {
        assert_eq!(
            self.tip,
            self.headers
                .last()
                .map(|h| *h.hash())
                .unwrap_or(*DEFAULT_BLOCKHASH)
        );
        &self.tip
    }

    pub fn len(&self) -> usize {
        self.headers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }

    pub fn iter(&self) -> slice::Iter<HeaderEntry> {
        self.headers.iter()
    }

    /// Get the Median Time Past
    pub fn get_mtp(&self, height: usize) -> u32 {
        // Use the timestamp as the mtp of the genesis block.
        // Matches bitcoind's behaviour: bitcoin-cli getblock `bitcoin-cli getblockhash 0` | jq '.time == .mediantime'
        if height == 0 {
            self.headers.get(0).unwrap().header.time
        } else if height > self.len() - 1 {
            0
        } else {
            let mut timestamps = (height.saturating_sub(MTP_SPAN - 1)..=height)
                .map(|p_height| self.headers.get(p_height).unwrap().header.time)
                .collect::<Vec<_>>();
            timestamps.sort_unstable();
            timestamps[timestamps.len() / 2]
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct BlockStatus {
    pub in_best_chain: bool,
    pub height: Option<usize>,
    pub next_best: Option<BlockHash>,
}

impl BlockStatus {
    pub fn confirmed(height: usize, next_best: Option<BlockHash>) -> BlockStatus {
        BlockStatus {
            in_best_chain: true,
            height: Some(height),
            next_best,
        }
    }

    pub fn orphaned() -> BlockStatus {
        BlockStatus {
            in_best_chain: false,
            height: None,
            next_best: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BlockMeta {
    #[serde(alias = "nTx")]
    pub tx_count: u32,
    pub size: u32,
    pub weight: u32,
}

pub struct BlockHeaderMeta {
    pub header_entry: HeaderEntry,
    pub meta: BlockMeta,
    pub mtp: u32,
}

impl From<&BlockEntry> for BlockMeta {
    fn from(b: &BlockEntry) -> BlockMeta {
        let weight = b.block.weight();
        #[cfg(not(feature = "liquid"))] // rust-bitcoin has a wrapper Weight type
        let weight = weight.to_wu();

        BlockMeta {
            tx_count: b.block.txdata.len() as u32,
            // To retain DB compatibility, block weights are converted from the u64
            // representation used as of rust-bitcoin v0.30 back to a u32. This is OK
            // because u32::MAX is far above MAX_BLOCK_WEIGHT.
            weight: weight as u32,
            size: b.size,
        }
    }
}

impl BlockMeta {
    pub fn parse_getblock(val: ::serde_json::Value) -> Result<BlockMeta> {
        Ok(BlockMeta {
            tx_count: val
                .get("nTx")
                .chain_err(|| "missing nTx")?
                .as_f64()
                .chain_err(|| "nTx not a number")? as u32,
            size: val
                .get("size")
                .chain_err(|| "missing size")?
                .as_f64()
                .chain_err(|| "size not a number")? as u32,
            weight: val
                .get("weight")
                .chain_err(|| "missing weight")?
                .as_f64()
                .chain_err(|| "weight not a number")? as u32,
        })
    }
}
