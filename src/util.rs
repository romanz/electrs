use bitcoin::blockdata::block::BlockHeader;
use bitcoin::util::hash::Sha256dHash;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt;
use std::iter::FromIterator;
use time;

pub type Bytes = Vec<u8>;
pub type HeaderMap = HashMap<Sha256dHash, BlockHeader>;

// TODO: consolidate serialization/deserialize code for bincode/bitcoin.
const HASH_LEN: usize = 32;
pub const HASH_PREFIX_LEN: usize = 8;

pub type FullHash = [u8; HASH_LEN];
pub type HashPrefix = [u8; HASH_PREFIX_LEN];

pub fn hash_prefix(hash: &[u8]) -> HashPrefix {
    array_ref![hash, 0, HASH_PREFIX_LEN].clone()
}

pub fn full_hash(hash: &[u8]) -> FullHash {
    array_ref![hash, 0, HASH_LEN].clone()
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

impl fmt::Debug for HeaderEntry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let last_block_time = time::at_utc(time::Timespec::new(self.header().time as i64, 0))
            .rfc3339()
            .to_string();
        write!(
            f,
            "best={} height={} @ {}",
            self.hash(),
            self.height(),
            last_block_time,
        )
    }
}

pub struct HeaderList {
    headers: Vec<HeaderEntry>,
    tip: Sha256dHash,
}

impl HeaderList {
    pub fn build(mut header_map: HeaderMap, mut blockhash: Sha256dHash) -> HeaderList {
        let null_hash = Sha256dHash::default();
        let tip = blockhash;
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
        if !header_map.is_empty() {
            warn!("{} orphaned blocks: {:?}", header_map.len(), header_map);
        }
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
            tip: tip,
        }
    }

    pub fn empty() -> HeaderList {
        HeaderList {
            headers: vec![],
            tip: Sha256dHash::default(),
        }
    }

    pub fn equals(&self, other: &HeaderList) -> bool {
        self.headers.last() == other.headers.last()
    }

    pub fn headers(&self) -> &[HeaderEntry] {
        &self.headers
    }

    pub fn tip(&self) -> Sha256dHash {
        assert_eq!(
            self.tip,
            self.headers
                .last()
                .map(|h| *h.hash())
                .unwrap_or(Sha256dHash::default())
        );
        self.tip
    }

    pub fn height(&self) -> usize {
        self.headers.len() - 1
    }

    pub fn as_map(&self) -> HeaderMap {
        HeaderMap::from_iter(self.headers.iter().map(|entry| (entry.hash, entry.header)))
    }

    pub fn get_missing_headers(&self, existing_headers_map: &HeaderMap) -> Vec<&HeaderEntry> {
        let missing: Vec<&HeaderEntry> = self.headers()
            .iter()
            .filter(|entry| !existing_headers_map.contains_key(&entry.hash()))
            .collect();
        info!("{:?} ({} left to index)", self, missing.len());
        missing
    }
}
