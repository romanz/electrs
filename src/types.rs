use crate::bitcoin::hashes::{hash_newtype, sha256};

hash_newtype! {
    /// https://electrum-protocol.readthedocs.io/en/latest/protocol-basics.html#status
    pub struct StatusHash(sha256::Hash);
}

/// The different authentication methods for the client.
#[derive(Clone, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub enum Auth {
    UserPass(String, String),
    CookieFile(std::path::PathBuf),
}
