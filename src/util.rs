use bitcoin::blockdata::block::BlockHeader;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::util::hash::Sha256dHash;
use std::collections::HashMap;
use std::fmt;
use std::iter::FromIterator;
use std::slice;
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::thread;
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
    heights: HashMap<Sha256dHash, usize>,
    tip: Sha256dHash,
}

impl HeaderList {
    pub fn empty() -> HeaderList {
        HeaderList {
            headers: vec![],
            heights: HashMap::new(),
            tip: Sha256dHash::default(),
        }
    }

    pub fn order(&self, new_headers: Vec<BlockHeader>) -> Vec<HeaderEntry> {
        // header[i] -> header[i-1] (i.e. header.last() is the tip)
        struct HashedHeader {
            blockhash: Sha256dHash,
            header: BlockHeader,
        }
        let hashed_headers =
            Vec::<HashedHeader>::from_iter(new_headers.into_iter().map(|header| HashedHeader {
                blockhash: header.bitcoin_hash(),
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
        let null_hash = Sha256dHash::default();
        let new_height: usize = if prev_blockhash == null_hash {
            0
        } else {
            self.header_by_blockhash(&prev_blockhash)
                .expect(&format!("{} is not part of the blockchain", prev_blockhash))
                .height()
                + 1
        };
        (new_height..)
            .zip(hashed_headers.into_iter())
            .map(|(height, hashed_header)| HeaderEntry {
                height: height,
                hash: hashed_header.blockhash,
                header: hashed_header.header,
            }).collect()
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
                    Sha256dHash::default()
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
        self.headers.split_off(new_height); // keep [0..new_height) entries
        for new_header in new_headers {
            let height = new_header.height();
            assert_eq!(height, self.headers.len());
            self.tip = *new_header.hash();
            self.headers.push(new_header);
            self.heights.insert(self.tip, height);
        }
    }

    pub fn header_by_blockhash(&self, blockhash: &Sha256dHash) -> Option<&HeaderEntry> {
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

    pub fn tip(&self) -> &Sha256dHash {
        assert_eq!(
            self.tip,
            self.headers
                .last()
                .map(|h| *h.hash())
                .unwrap_or(Sha256dHash::default())
        );
        &self.tip
    }

    pub fn len(&self) -> usize {
        self.headers.len()
    }

    pub fn iter(&self) -> slice::Iter<HeaderEntry> {
        self.headers.iter()
    }
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



use secp256k1::{Secp256k1};
use secp256k1::key::PublicKey;
use bitcoin::{Script};
use bitcoin::util::address::{Address,Payload};
use bitcoin::util::hash::Hash160;
use bitcoin_bech32::{WitnessProgram,u5};
use bitcoin::network::constants::Network;
use bitcoin_bech32::constants::Network as B32Network;

// @XXX we can't use any of the Address:p2{...}h utility methods, since they expect the pre-image data, which we don't have.
// we must instead create the Payload manually, which results in code duplication with the p2{...}h methods, especially for witness programs.
// ideally, this should be implemented as part of the rust-bitcoin lib.
pub fn script_to_address(script: &Script, network: &Network) -> Option<String> {
    let payload = if script.is_p2pk() {
        Some(Payload::Pubkey(PublicKey::from_slice(&Secp256k1::without_caps(), &script[1..66]).unwrap()))
    } else if script.is_p2pkh() {
        Some(Payload::PubkeyHash(Hash160::from(&script[3..23])))
    } else if script.is_p2sh() {
        Some(Payload::ScriptHash(Hash160::from(&script[2..22])))
    } else if script.is_v0_p2wpkh() {
        Some(Payload::WitnessProgram(WitnessProgram::new(u5::try_from_u8(0).expect("0<32"),
                                                         script[2..22].to_vec(),
                                                         to_bech_network(network)).unwrap()))
    } else if script.is_v0_p2wsh() {
        Some(Payload::WitnessProgram(WitnessProgram::new(u5::try_from_u8(0).expect("0<32"),
                                                         script[2..34].to_vec(),
                                                         to_bech_network(network)).unwrap()))
    } else { None };

    Some(Address { payload: payload?, network: *network }.to_string())
}

fn to_bech_network (network: &Network) -> B32Network {
    match network {
        Network::Bitcoin => B32Network::Bitcoin,
        Network::Testnet => B32Network::Testnet,
        Network::Regtest => B32Network::Regtest,
    }
}

