use anyhow::{bail, Context, Result};
use futures::sink::SinkExt;
use rayon::prelude::*;
use serde_derive::{Deserialize, Serialize};
use serde_json::{from_value, json, Value};
use std::{
    cmp::min,
    collections::hash_map::Entry::{Occupied, Vacant},
    collections::{HashMap, HashSet},
    sync::RwLock,
    time::{Duration, Instant},
};

use crate::mempool::{Mempool, MempoolEntry};
use crate::util::{spawn, unbounded, Receiver, Sender};

use electrs_index::{
    bitcoin::{
        consensus::{deserialize, serialize},
        hashes::{
            borrow_slice_impl, hash_newtype,
            hex::{FromHex, ToHex},
            hex_fmt_impl, index_impl, serde_impl, sha256, Hash,
        },
        BlockHash, Transaction, TxMerkleNode, Txid,
    },
    Confirmed, Daemon, Histogram, Index, Metrics, ScriptHash,
};

const ELECTRS_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: &str = "1.4";
const BANNER: &str = "Welcome to the (WIP) Electrum Rust Server!";

#[derive(Debug, Deserialize, Serialize, Clone)]
struct TxEntry {
    #[serde(rename = "tx_hash")]
    txid: Txid,
    height: isize,
    fee: Option<u64>, // in satoshis
}

impl TxEntry {
    fn unconfirmed(entry: &MempoolEntry, mempool: &Mempool) -> Self {
        let inputs = &entry.tx.input;
        let has_unconfirmed_inputs = inputs
            .iter()
            .any(|txi| mempool.get(&txi.previous_output.txid).is_some());
        Self {
            txid: entry.txid,
            height: if has_unconfirmed_inputs { -1 } else { 0 },
            fee: Some(entry.fee.as_sat()),
        }
    }

    fn confirmed(c: &Confirmed) -> Self {
        Self {
            txid: c.txid,
            height: c.height as isize,
            fee: None,
        }
    }

    fn is_confirmed(&self) -> bool {
        self.height > 0
    }
}

hash_newtype!(StatusHash, sha256::Hash, 32, doc = "SHA256(status)", false);

impl StatusHash {
    fn new(entries: &[TxEntry]) -> Option<Self> {
        if entries.is_empty() {
            None
        } else {
            let status = entries
                .iter()
                .map(|entry| format!("{}:{}:", entry.txid, entry.height))
                .collect::<Vec<String>>()
                .join("");
            let hash = StatusHash::hash(&status.as_bytes());
            trace!("{} => {}", status, hash);
            Some(hash)
        }
    }
}

struct Status {
    entries: Vec<TxEntry>,
    hash: Option<StatusHash>,
    tip: BlockHash,
}

impl Status {
    fn new(mut entries: Vec<TxEntry>, tip: BlockHash) -> Self {
        let mut txids = HashSet::new();
        entries = entries
            .into_iter()
            .filter(|e| txids.insert(e.txid)) // deduplicate txids, assuming the latter are from mempool
            .collect();
        let hash = StatusHash::new(&entries);
        Self { entries, hash, tip }
    }

    fn confirmed(&self) -> Vec<TxEntry> {
        self.entries
            .iter()
            .filter(|e| e.is_confirmed())
            .cloned()
            .collect()
    }
}

pub(crate) struct Subscription {
    tip: Option<BlockHash>,
    status: HashMap<ScriptHash, Status>,
}

impl Subscription {
    pub(crate) fn new() -> Self {
        Self {
            tip: None,
            status: HashMap::new(),
        }
    }
}

fn notification(method: &str, params: &[Value]) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

#[derive(Debug, Deserialize, Serialize)]
struct Request {
    id: Value,
    jsonrpc: String,
    method: String,

    #[serde(default)]
    params: Value,
}

async fn wait_for_new_blocks(
    mut tip: BlockHash,
    daemon: Daemon,
    mut new_block_tx: Sender<BlockHash>,
) -> Result<()> {
    let mut new_tip = daemon.get_best_block_hash()?;
    loop {
        if tip != new_tip {
            tip = new_tip;
            new_block_tx.send(tip).await?;
        }
        new_tip = daemon
            .wait_for_new_block(Duration::from_secs(60))
            .context("failed to wait for new block")?;
    }
}

struct Stats {
    rpc_duration: Histogram,
    sync_duration: Histogram,
}

impl Stats {
    fn new(metrics: &Metrics) -> Self {
        Self {
            rpc_duration: metrics.histogram_vec(
                "rpc_duration",
                "RPC handling duration (in seconds)",
                &["name"],
            ),
            sync_duration: metrics.histogram_vec(
                "sync_duration",
                "RPC sync duration (in seconds)",
                &["name"],
            ),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TxGetArgs {
    Txid((Txid,)),
    TxidVerbose(Txid, bool),
}

impl From<TxGetArgs> for (Txid, bool) {
    fn from(args: TxGetArgs) -> Self {
        match args {
            TxGetArgs::Txid((txid,)) => (txid, false),
            TxGetArgs::TxidVerbose(txid, verbose) => (txid, verbose),
        }
    }
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
#[serde(untagged)]
enum ClientVersion {
    Single(String),
    Range(String, String),
}

pub(crate) struct Rpc {
    index: Index,
    daemon: Daemon,
    mempool: Mempool,
    tx_cache: RwLock<HashMap<Txid, Transaction>>,
    stats: Stats,
    // mempool polling
    next_poll: Instant,
    poll_period: Duration,
}

impl Rpc {
    pub(crate) fn new(index: Index, daemon: Daemon, metrics: &Metrics) -> Result<Self> {
        let mut rpc = Self {
            index,
            daemon,
            mempool: Mempool::empty(metrics),
            tx_cache: RwLock::new(HashMap::new()),
            stats: Stats::new(metrics),
            next_poll: Instant::now(),
            poll_period: Duration::from_secs(1),
        };
        rpc.sync_index().context("failed to sync with bitcoind")?;
        info!("loaded {} mempool txs", rpc.mempool.count());
        Ok(rpc)
    }

    pub(crate) fn sync_index(&mut self) -> Result<BlockHash> {
        let result = self
            .stats
            .sync_duration
            .observe_duration("index", || self.index.update(&self.daemon));
        self.next_poll = Instant::now();
        self.sync_mempool(); // remove confirmed transactions from mempool
        result
    }

    pub(crate) fn sync_mempool(&mut self) {
        let sync_duration = self.stats.sync_duration.clone();
        sync_duration.observe_duration("mempool", || {
            let now = Instant::now();
            if now <= self.next_poll {
                return;
            }
            self.next_poll = now + self.poll_period;
            if let Err(e) = self.mempool.update(&self.daemon) {
                warn!("failed to sync mempool: {:?}", e);
            }
        })
    }

    pub(crate) fn start_waiter(&self) -> Result<Receiver<BlockHash>> {
        let (new_block_tx, new_block_rx) = unbounded::<BlockHash>();
        let current_tip = match self.index.map().chain().last() {
            None => bail!("empty chain"),
            Some(tip) => *tip,
        };
        spawn(
            "waiter",
            wait_for_new_blocks(current_tip, self.daemon.reconnect()?, new_block_tx),
        );
        Ok(new_block_rx)
    }

    pub(crate) fn notify(&self, subscription: &mut Subscription) -> Result<Vec<Value>> {
        self.stats.sync_duration.observe_duration("notify", || {
            let mut result = vec![];

            let map = self.index.map();
            let chain = map.chain();
            let current_tip = match chain.last() {
                None => bail!("empty chain"),
                Some(tip) => *tip,
            };
            if let Some(last_tip) = subscription.tip {
                if current_tip != last_tip {
                    let header = serialize(map.get_by_hash(&current_tip).unwrap());
                    result.push(notification(
                        "blockchain.headers.subscribe",
                        &[json!({"hex": header.to_hex(), "height": chain.len() - 1})],
                    ));
                    subscription.tip = Some(current_tip);
                }
            };
            drop(map);

            for (scripthash, status) in subscription.status.iter_mut() {
                let current_hash = status.hash;
                let (mut entries, tip) = if status.tip == current_tip {
                    (status.confirmed(), status.tip)
                } else {
                    self.get_confirmed(&scripthash)?
                };
                entries.extend(self.get_unconfirmed(&scripthash));
                *status = Status::new(entries, tip);
                if current_hash != status.hash {
                    result.push(notification(
                        "blockchain.scripthash.subscribe",
                        &[json!(scripthash), json!(status.hash)],
                    ));
                }
            }
            Ok(result)
        })
    }

    fn headers_subscribe(&self, subscription: &mut Subscription) -> Result<Value> {
        let map = self.index.map();
        let chain = map.chain();
        Ok(match chain.last() {
            None => bail!("empty chain"),
            Some(tip) => {
                subscription.tip = Some(*tip);
                let header = serialize(map.get_by_hash(tip).unwrap());
                let height = chain.len() - 1;
                json!({"hex": header.to_hex(), "height": height})
            }
        })
    }

    fn block_header(&self, (height,): (usize,)) -> Result<Value> {
        let map = self.index.map();
        let chain = map.chain();
        match chain.get(height) {
            None => bail!("no header at {}", height),
            Some(hash) => Ok(json!(serialize(map.get_by_hash(hash).unwrap()).to_hex())),
        }
    }

    fn block_headers(&self, (start_height, count): (usize, usize)) -> Result<Value> {
        let map = self.index.map();
        let chain = map.chain();

        let max: usize = 2016;
        let count = min(min(count, max), chain.len() - start_height);
        let hex_headers: Vec<String> = chain
            .get(start_height..start_height + count)
            .unwrap()
            .iter()
            .map(|hash| serialize(map.get_by_hash(hash).unwrap()).to_hex())
            .collect();
        Ok(json!({"count": count, "hex": hex_headers.join(""), "max": max}))
    }

    fn estimate_fee(&self, (nblocks,): (u16,)) -> Result<Value> {
        Ok(self
            .daemon
            .estimate_fee(nblocks)?
            .map(|a| json!(a.as_btc()))
            .unwrap_or_else(|| json!(-1)))
    }

    fn scripthash_get_history(
        &self,
        subscription: &Subscription,
        (scripthash,): (ScriptHash,),
    ) -> Result<Value> {
        match subscription.status.get(&scripthash) {
            Some(status) => Ok(json!(status.entries)),
            None => bail!("no subscription for scripthash"),
        }
    }

    fn scripthash_subscribe(
        &self,
        subscription: &mut Subscription,
        (scripthash,): (ScriptHash,),
    ) -> Result<Value> {
        let (mut entries, tip) = self.get_confirmed(&scripthash)?;
        entries.extend(self.get_unconfirmed(&scripthash));
        let status = Status::new(entries, tip);
        let hash = status.hash;
        subscription.status.insert(scripthash, status);
        Ok(json!(hash))
    }

    fn get_confirmed(&self, scripthash: &ScriptHash) -> Result<(Vec<TxEntry>, BlockHash)> {
        let result = self
            .index
            .lookup_by_scripthash(&scripthash, &self.daemon)
            .context("index lookup failed")?;
        let mut confirmed: Vec<Confirmed> = result
            .readers
            .into_par_iter()
            .map(|r| r.read())
            .collect::<Result<_>>()
            .context("transaction reading failed")?;
        confirmed.sort_by_key(|c| (c.height, c.file_offset));
        let entries: Vec<TxEntry> = confirmed.iter().map(TxEntry::confirmed).collect();
        let mut tx_cache = self.tx_cache.write().unwrap();
        for c in confirmed {
            tx_cache.entry(c.txid).or_insert(c.tx);
        }
        Ok((entries, result.tip))
    }

    fn get_unconfirmed(&self, scripthash: &ScriptHash) -> Vec<TxEntry> {
        let entries: Vec<&MempoolEntry> = self.mempool.lookup(*scripthash);
        let mut unconfirmed: Vec<TxEntry> = entries
            .iter()
            .map(|e| TxEntry::unconfirmed(e, &self.mempool))
            .collect();
        unconfirmed.sort_by_key(|u| u.txid);
        let getter = |txid| match self.mempool.get(txid) {
            Some(e) => e.tx.clone(),
            None => panic!("missing mempool entry {}", txid),
        };
        let mut tx_cache = self.tx_cache.write().unwrap();
        for u in &unconfirmed {
            tx_cache.entry(u.txid).or_insert_with(|| getter(&u.txid));
        }
        unconfirmed
    }

    fn transaction_get_confirmed(&self, txid: &Txid) -> Result<Option<Confirmed>> {
        let result = self.index.lookup_by_txid(&txid, &self.daemon)?;
        let confirmed: Vec<Confirmed> = result
            .readers
            .into_par_iter()
            .filter_map(|r| {
                let result: Result<Confirmed> = r.read();
                match result.as_ref().map(|confirmed| confirmed.txid) {
                    Ok(read_txid) if read_txid == *txid => Some(result),
                    Ok(read_txid) => {
                        warn!("read {}, expecting {}", read_txid, txid);
                        None
                    }
                    Err(_) => Some(result),
                }
            })
            .collect::<Result<Vec<Confirmed>>>()
            .context("transaction reading failed")?;
        Ok(match confirmed.len() {
            0 | 1 => confirmed.into_iter().next(),
            _ => panic!("duplicate transactions: {:?}", confirmed),
        })
    }

    fn transaction_get(&self, args: TxGetArgs) -> Result<Value> {
        let (txid, verbose) = args.into();
        if verbose {
            let block_hash = self
                .transaction_get_confirmed(&txid)?
                .map(|confirmed| confirmed.header.block_hash());
            return self.daemon.get_raw_transaction(&txid, block_hash.as_ref());
        }
        let mut cache = self.tx_cache.write().unwrap();
        let tx_bytes = {
            match cache.entry(txid) {
                Occupied(entry) => serialize(entry.get()),
                Vacant(entry) => {
                    debug!("tx {} is not cached", txid);
                    match self.transaction_get_confirmed(&txid)? {
                        Some(confirmed) => serialize(entry.insert(confirmed.tx)),
                        None => {
                            debug!("unconfirmed transaction {}", txid);
                            match self.mempool.get(&txid) {
                                Some(e) => serialize(&e.tx),
                                None => bail!("missing transaction {}", txid),
                            }
                        }
                    }
                }
            }
        };
        Ok(json!(tx_bytes.to_hex()))
    }

    fn transaction_get_merkle(&self, (txid, height): (Txid, usize)) -> Result<Value> {
        let blockhash = {
            let map = self.index.map();
            let chain = map.chain();
            match chain.get(height) {
                None => bail!("missing block at {}", height),
                Some(blockhash) => *blockhash,
            }
        };
        let block = self.index.block_reader(&blockhash, &self.daemon)?.read()?;
        let txids: Vec<Txid> = block.txdata.into_iter().map(|tx| tx.txid()).collect();
        let pos = match txids.iter().position(|current_txid| *current_txid == txid) {
            None => bail!("missing tx {} at block {}", txid, blockhash),
            Some(pos) => pos,
        };
        let nodes: Vec<TxMerkleNode> = txids
            .iter()
            .map(|txid| TxMerkleNode::from_hash(txid.as_hash()))
            .collect();
        let merkle: Vec<String> = create_merkle_branch(nodes, pos)
            .into_iter()
            .map(|node| node.to_hex())
            .collect();
        Ok(json!({"block_height": height, "pos": pos, "merkle": merkle}))
    }

    fn transaction_broadcast(&mut self, (tx_hex,): (String,)) -> Result<Value> {
        let tx_bytes = Vec::from_hex(&tx_hex).context("non-hex transaction")?;
        let tx: Transaction = deserialize(&tx_bytes).context("invalid transaction")?;
        let txid = self
            .daemon
            .broadcast(&tx)
            .with_context(|| format!("failed to broadcast transaction: {}", tx.txid()))?;
        self.next_poll = Instant::now();
        self.sync_mempool();
        Ok(json!(txid))
    }

    fn relayfee(&self) -> Result<Value> {
        Ok(json!(self.daemon.get_relay_fee()?.as_btc())) // [BTC/kB]
    }

    fn get_fee_histogram(&self) -> Result<Value> {
        Ok(json!(self.mempool.histogram()))
    }

    fn version(&self, (client_id, client_version): (String, ClientVersion)) -> Result<Value> {
        match client_version {
            ClientVersion::Single(v) if v == PROTOCOL_VERSION => (),
            _ => {
                bail!(
                    "{} requested {:?}, server supports {}",
                    client_id,
                    client_version,
                    PROTOCOL_VERSION
                );
            }
        };
        let server_id = format!("electrs/{}", ELECTRS_VERSION);
        Ok(json!([server_id, PROTOCOL_VERSION]))
    }

    pub(crate) fn handle_request(&mut self, sub: &mut Subscription, value: Value) -> Result<Value> {
        let rpc_duration = self.stats.rpc_duration.clone();
        let req_str = value.to_string();
        let mut req: Request =
            from_value(value).with_context(|| format!("invalid request: {}", req_str))?;
        let params = req.params.take();
        rpc_duration.observe_duration(req.method.as_str(), || {
            let result = match req.method.as_str() {
                "blockchain.scripthash.get_history" => {
                    self.scripthash_get_history(sub, from_value(params)?)
                }
                "blockchain.scripthash.subscribe" => {
                    self.scripthash_subscribe(sub, from_value(params)?)
                }
                "blockchain.transaction.broadcast" => {
                    self.transaction_broadcast(from_value(params)?)
                }
                "blockchain.transaction.get" => self.transaction_get(from_value(params)?),
                "blockchain.transaction.get_merkle" => {
                    self.transaction_get_merkle(from_value(params)?)
                }
                "server.banner" => Ok(json!(BANNER)),
                "server.donation_address" => Ok(Value::Null),
                "server.peers.subscribe" => Ok(json!([])),
                "blockchain.block.header" => self.block_header(from_value(params)?),
                "blockchain.block.headers" => self.block_headers(from_value(params)?),
                "blockchain.estimatefee" => self.estimate_fee(from_value(params)?),
                "blockchain.headers.subscribe" => self.headers_subscribe(sub),
                "blockchain.relayfee" => self.relayfee(),
                "mempool.get_fee_histogram" => self.get_fee_histogram(),
                "server.ping" => Ok(Value::Null),
                "server.version" => self.version(from_value(params)?),
                &_ => bail!("unknown method '{}' with {}", req.method, params,),
            };

            Ok(match result {
                Ok(value) => json!({"jsonrpc": req.jsonrpc, "id": req.id, "result": value}),
                Err(err) => {
                    let msg = format!("RPC {} failed: {:#}", req_str, err);
                    warn!("{}", msg);
                    let error = json!({"code": 1, "message": msg});
                    json!({"jsonrpc": req.jsonrpc, "id": req.id, "error": error})
                }
            })
        })
    }
}

fn create_merkle_branch<T: Hash>(mut hashes: Vec<T>, mut index: usize) -> Vec<T> {
    let mut result = vec![];
    while hashes.len() > 1 {
        if hashes.len() % 2 != 0 {
            let last = *hashes.last().unwrap();
            hashes.push(last);
        }
        index = if index % 2 == 0 { index + 1 } else { index - 1 };
        result.push(hashes[index]);
        index /= 2;
        hashes = hashes
            .chunks(2)
            .map(|pair| {
                let left = pair[0];
                let right = pair[1];
                let input = [&left[..], &right[..]].concat();
                <T as Hash>::hash(&input)
            })
            .collect()
    }
    result
}

#[cfg(test)]
mod tests {
    use crate::rpc::StatusHash;
    use electrs_index::bitcoin::hashes::Hash;
    use serde_json::json;

    #[test]
    fn test_status_hash() {
        let status = "fb94cc7696fd077921a5918ad3ba973178845d800f564ac718b40f28b54e6091:650954:a7a3c9b47bf8f18136eca88386eed331971e6b2c0c41a5ea29cea88f7511564e:650958:6cc9451739098b70afc5d0e2cc3965136e87cb16dfd514f20d8252b3e6cbe565:650958:".as_bytes();
        let hash = StatusHash::hash(status);
        let hex = "8187fc643ddca88968ee123770078e3304cf1dcd889edf6b4b1026980774f8d9";
        assert_eq!(format!("{}", hash), hex);
        assert_eq!(json!(hash).to_string(), format!("\"{}\"", hex));
    }
}
