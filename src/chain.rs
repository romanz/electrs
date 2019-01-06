#[cfg(not(feature = "liquid"))] // use regular Bitcoin data structures
pub use bitcoin::{Block, BlockHeader, OutPoint, Transaction, TxIn, TxOut};

#[cfg(feature = "liquid")]
pub use elements::{confidential, Block, BlockHeader, OutPoint, Transaction, TxIn, TxOut};
