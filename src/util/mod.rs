mod block;
mod merkle;
mod script;
mod transaction;

pub mod fees;

pub use self::block::{BlockHeaderMeta, BlockId, BlockMeta, BlockStatus, HeaderEntry, HeaderList};
pub use self::merkle::{get_header_merkle_proof, get_id_from_pos, get_tx_merkle_proof};
pub use self::script::{get_innerscripts, get_script_asm, script_to_address};
pub use self::transaction::{has_prevout, is_coinbase, is_spendable, TransactionStatus, TxInput};

use std::collections::HashMap;
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::thread;

use crate::chain::BlockHeader;
use bitcoin::hashes::sha256d::Hash as Sha256dHash;

pub type Bytes = Vec<u8>;
pub type HeaderMap = HashMap<Sha256dHash, BlockHeader>;

// TODO: consolidate serialization/deserialize code for bincode/bitcoin.
const HASH_LEN: usize = 32;

pub type FullHash = [u8; HASH_LEN];

pub fn full_hash(hash: &[u8]) -> FullHash {
    array_ref![hash, 0, HASH_LEN].clone()
}

pub struct SyncChannel<T> {
    tx: SyncSender<T>,
    rx: Receiver<T>,
}

impl<T> SyncChannel<T> {
    pub fn new(size: usize) -> SyncChannel<T> {
        let (tx, rx) = sync_channel(size);
        SyncChannel { tx, rx }
    }

    pub fn sender(&self) -> SyncSender<T> {
        self.tx.clone()
    }

    pub fn receiver(&self) -> &Receiver<T> {
        &self.rx
    }

    pub fn into_receiver(self) -> Receiver<T> {
        self.rx
    }
}

pub struct Channel<T> {
    tx: Sender<T>,
    rx: Receiver<T>,
}

impl<T> Channel<T> {
    pub fn new() -> Channel<T> {
        let (tx, rx) = channel();
        Channel { tx, rx }
    }

    pub fn sender(&self) -> Sender<T> {
        self.tx.clone()
    }

    pub fn receiver(&self) -> &Receiver<T> {
        &self.rx
    }

    pub fn into_receiver(self) -> Receiver<T> {
        self.rx
    }
}

pub fn spawn_thread<F, T>(name: &str, f: F) -> thread::JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(f)
        .unwrap()
}
