use crate::bitcoin::hashes::{hash_newtype, sha256};

hash_newtype! {
    /// https://electrum-protocol.readthedocs.io/en/latest/protocol-basics.html#status
    pub struct StatusHash(sha256::Hash);
}
