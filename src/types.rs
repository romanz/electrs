use std::collections::HashMap;

pub use bitcoin::blockdata::block::BlockHeader;
pub use bitcoin::util::hash::Sha256dHash;

pub type Bytes = Vec<u8>;
pub type HeaderMap = HashMap<Sha256dHash, BlockHeader>;
