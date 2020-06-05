use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;

use bitcoin::hashes::hex::{FromHex, ToHex};
use bitcoin::hashes::sha256d::Hash as Sha256dHash;
use bitcoin::Txid;
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use error_chain::ChainedError;
use hex;
use serde_json::{from_str, Value};

#[cfg(not(feature = "liquid"))]
use bitcoin::consensus::encode::serialize;
#[cfg(feature = "liquid")]
use elements::encode::serialize;

use crate::config::Config;
use crate::errors::*;
use crate::metrics::{Gauge, HistogramOpts, HistogramVec, MetricOpts, Metrics};
use crate::new_index::Query;
use crate::util::electrum_merkle::{get_header_merkle_proof, get_id_from_pos, get_tx_merkle_proof};
use crate::util::{full_hash, spawn_thread, BlockId, Channel, FullHash, HeaderEntry, SyncChannel};

const ELECTRS_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: &str = "1.4";

// TODO: Sha256dHash should be a generic hash-container (since script hash is single SHA256)
fn hash_from_value(val: Option<&Value>) -> Result<Sha256dHash> {
    let script_hash = val.chain_err(|| "missing hash")?;
    let script_hash = script_hash.as_str().chain_err(|| "non-string hash")?;
    let script_hash = Sha256dHash::from_hex(script_hash).chain_err(|| "non-hex hash")?;
    Ok(script_hash)
}

fn usize_from_value(val: Option<&Value>, name: &str) -> Result<usize> {
    let val = val.chain_err(|| format!("missing {}", name))?;
    let val = val.as_u64().chain_err(|| format!("non-integer {}", name))?;
    Ok(val as usize)
}

fn usize_from_value_or(val: Option<&Value>, name: &str, default: usize) -> Result<usize> {
    if val.is_none() {
        return Ok(default);
    }
    usize_from_value(val, name)
}

fn bool_from_value(val: Option<&Value>, name: &str) -> Result<bool> {
    let val = val.chain_err(|| format!("missing {}", name))?;
    let val = val.as_bool().chain_err(|| format!("not a bool {}", name))?;
    Ok(val)
}

fn bool_from_value_or(val: Option<&Value>, name: &str, default: bool) -> Result<bool> {
    if val.is_none() {
        return Ok(default);
    }
    bool_from_value(val, name)
}

// FIXME: implement caching and delta updates
// FIXME: ensure stable ordering
fn get_status_hash(txs: Vec<(Txid, Option<BlockId>)>) -> Option<FullHash> {
    if txs.is_empty() {
        None
    } else {
        let mut hash = FullHash::default();
        let mut sha2 = Sha256::new();
        for (txid, blockid) in txs {
            // TODO: use height of 0 to indicate an unconfirmed tx with confirmed inputs, or -1 for unconfirmed tx with unconfirmed inputs
            let part = format!("{}:{}:", txid.to_hex(), blockid.map_or(0, |b| b.height));
            sha2.input(part.as_bytes());
        }
        sha2.result(&mut hash);
        Some(hash)
    }
}

struct Connection {
    query: Arc<Query>,
    last_header_entry: Option<HeaderEntry>,
    status_hashes: HashMap<Sha256dHash, Value>, // ScriptHash -> StatusHash
    stream: TcpStream,
    addr: SocketAddr,
    chan: SyncChannel<Message>,
    stats: Arc<Stats>,
    txs_limit: usize,
}

impl Connection {
    pub fn new(
        query: Arc<Query>,
        stream: TcpStream,
        addr: SocketAddr,
        stats: Arc<Stats>,
        txs_limit: usize,
    ) -> Connection {
        Connection {
            query,
            last_header_entry: None, // disable header subscription for now
            status_hashes: HashMap::new(),
            stream,
            addr,
            chan: SyncChannel::new(10),
            stats,
            txs_limit,
        }
    }

    fn blockchain_headers_subscribe(&mut self) -> Result<Value> {
        let entry = self.query.chain().best_header();
        let hex_header = hex::encode(serialize(entry.header()));
        let result = json!({"hex": hex_header, "height": entry.height()});
        self.last_header_entry = Some(entry);
        Ok(result)
    }

    fn server_version(&self) -> Result<Value> {
        Ok(json!([
            format!("electrs-esplora {}", ELECTRS_VERSION),
            PROTOCOL_VERSION
        ]))
    }

    fn server_banner(&self) -> Result<Value> {
        Ok(json!(self.query.config().electrum_banner.clone()))
    }

    fn server_features(&self) -> Result<Value> {
        Ok(json!({
            "hosts": self.query.config().electrum_public_hosts,
            "server_version": format!("electrs-esplora {}", ELECTRS_VERSION),
            "genesis_hash": self.query.network().genesis_hash(),
            "protocol_min": PROTOCOL_VERSION,
            "protocol_max": PROTOCOL_VERSION,
            "hash_function": "sha256",
            "pruning": null,
        }))
    }

    fn server_donation_address(&self) -> Result<Value> {
        Ok(Value::Null)
    }

    fn server_peers_subscribe(&self) -> Result<Value> {
        Ok(json!([]))
    }

    fn server_add_peer(&self) -> Result<Value> {
        Ok(json!(false))
    }

    fn mempool_get_fee_histogram(&self) -> Result<Value> {
        Ok(json!(&self.query.mempool().backlog_stats().fee_histogram))
    }

    fn blockchain_block_header(&self, params: &[Value]) -> Result<Value> {
        let height = usize_from_value(params.get(0), "height")?;
        let cp_height = usize_from_value_or(params.get(1), "cp_height", 0)?;

        let raw_header_hex: String = self
            .query
            .chain()
            .header_by_height(height)
            .map(|entry| hex::encode(&serialize(entry.header())))
            .chain_err(|| "missing header")?;

        if cp_height == 0 {
            return Ok(json!(raw_header_hex));
        }
        let (branch, root) = get_header_merkle_proof(self.query.chain(), height, cp_height)?;

        let branch_vec: Vec<String> = branch.into_iter().map(|b| b.to_hex()).collect();

        Ok(json!({
            "header": raw_header_hex,
            "root": root.to_hex(),
            "branch": branch_vec
        }))
    }

    fn blockchain_block_headers(&self, params: &[Value]) -> Result<Value> {
        let start_height = usize_from_value(params.get(0), "start_height")?;
        let count = usize_from_value(params.get(1), "count")?;
        let cp_height = usize_from_value_or(params.get(2), "cp_height", 0)?;
        let heights: Vec<usize> = (start_height..(start_height + count)).collect();
        let headers: Vec<String> = heights
            .into_iter()
            .map(|height| {
                self.query
                    .chain()
                    .header_by_height(height)
                    .map(|entry| hex::encode(&serialize(entry.header())))
            })
            .collect::<Option<Vec<String>>>()
            .chain_err(|| "requested block headers not found")?;

        if count == 0 || cp_height == 0 {
            return Ok(json!({
                "count": headers.len(),
                "hex": headers.join(""),
                "max": 2016,
            }));
        }

        let (branch, root) =
            get_header_merkle_proof(self.query.chain(), start_height + (count - 1), cp_height)?;

        let branch_vec: Vec<String> = branch.into_iter().map(|b| b.to_hex()).collect();

        Ok(json!({
            "count": headers.len(),
            "hex": headers.join(""),
            "max": 2016,
            "root": root.to_hex(),
            "branch" : branch_vec
        }))
    }

    fn blockchain_estimatefee(&self, params: &[Value]) -> Result<Value> {
        let conf_target = usize_from_value(params.get(0), "blocks_count")?;
        let fee_rate = self
            .query
            .estimate_fee(conf_target as u16)
            .chain_err(|| format!("cannot estimate fee for {} blocks", conf_target))?;
        // convert from sat/b to BTC/kB, as expected by Electrum clients
        Ok(json!(fee_rate / 100_000f64))
    }

    fn blockchain_relayfee(&self) -> Result<Value> {
        let relayfee = self.query.get_relayfee()?;
        // convert from sat/b to BTC/kB, as expected by Electrum clients
        Ok(json!(relayfee / 100_000f64))
    }

    fn blockchain_scripthash_subscribe(&mut self, params: &[Value]) -> Result<Value> {
        let script_hash = hash_from_value(params.get(0)).chain_err(|| "bad script_hash")?;

        let history_txids = get_history(&self.query, &script_hash[..], self.txs_limit)?;
        let status_hash = get_status_hash(history_txids)
            .map_or(Value::Null, |h| json!(hex::encode(full_hash(&h[..]))));

        self.status_hashes.insert(script_hash, status_hash.clone());
        Ok(status_hash)
    }

    #[cfg(not(feature = "liquid"))]
    fn blockchain_scripthash_get_balance(&self, params: &[Value]) -> Result<Value> {
        let script_hash = hash_from_value(params.get(0)).chain_err(|| "bad script_hash")?;
        let (chain_stats, mempool_stats) = self.query.stats(&script_hash[..]);

        Ok(json!({
            "confirmed": chain_stats.funded_txo_sum - chain_stats.spent_txo_sum,
            "unconfirmed": mempool_stats.funded_txo_sum - mempool_stats.spent_txo_sum,
        }))
    }

    fn blockchain_scripthash_get_history(&self, params: &[Value]) -> Result<Value> {
        let script_hash = hash_from_value(params.get(0)).chain_err(|| "bad script_hash")?;
        let history_txids = get_history(&self.query, &script_hash[..], self.txs_limit)?;
        Ok(json!(Value::Array(
            history_txids
                .into_iter()
                .map(|(txid, blockid)| json!({"height": blockid.map_or(0, |b| b.height), "tx_hash": txid.to_hex()}))
                .collect()
        )))
    }

    fn blockchain_scripthash_listunspent(&self, params: &[Value]) -> Result<Value> {
        let script_hash = hash_from_value(params.get(0)).chain_err(|| "bad script_hash")?;
        let utxos = self.query.utxo(&script_hash[..]);
        Ok(json!(Value::Array(
            utxos
                .into_iter()
                .map(|utxo| json!({
                    "height": utxo.confirmed.map_or(0, |b| b.height),
                    "tx_pos": utxo.vout,
                    "tx_hash": utxo.txid.to_hex(),
                    "value": utxo.value,
                }))
                .collect()
        )))
    }

    fn blockchain_transaction_broadcast(&self, params: &[Value]) -> Result<Value> {
        let tx = params.get(0).chain_err(|| "missing tx")?;
        let tx = tx.as_str().chain_err(|| "non-string tx")?.to_string();
        let txid = self.query.broadcast_raw(&tx)?;
        if let Err(e) = self.chan.sender().try_send(Message::PeriodicUpdate) {
            warn!("failed to issue PeriodicUpdate after broadcast: {}", e);
        }
        Ok(json!(txid.to_hex()))
    }

    fn blockchain_transaction_get(&self, params: &[Value]) -> Result<Value> {
        let tx_hash = Txid::from(hash_from_value(params.get(0)).chain_err(|| "bad tx_hash")?);
        let verbose = match params.get(1) {
            Some(value) => value.as_bool().chain_err(|| "non-bool verbose value")?,
            None => false,
        };

        // FIXME: implement verbose support
        if verbose {
            bail!("verbose transactions are currently unsupported");
        }

        let tx = self
            .query
            .lookup_raw_txn(&tx_hash)
            .chain_err(|| "missing transaction")?;
        Ok(json!(hex::encode(tx)))
    }

    fn blockchain_transaction_get_merkle(&self, params: &[Value]) -> Result<Value> {
        let txid = Txid::from(hash_from_value(params.get(0)).chain_err(|| "bad tx_hash")?);
        let height = usize_from_value(params.get(1), "height")?;
        let blockid = self
            .query
            .chain()
            .tx_confirming_block(&txid)
            .ok_or_else(|| "tx not found or is unconfirmed")?;
        if blockid.height != height {
            bail!("invalid confirmation height provided");
        }
        let (merkle, pos) = get_tx_merkle_proof(self.query.chain(), &txid, &blockid.hash)
            .chain_err(|| "cannot create merkle proof")?;
        let merkle: Vec<String> = merkle.into_iter().map(|txid| txid.to_hex()).collect();
        Ok(json!({
                "block_height": blockid.height,
                "merkle": merkle,
                "pos": pos}))
    }

    fn blockchain_transaction_id_from_pos(&self, params: &[Value]) -> Result<Value> {
        let height = usize_from_value(params.get(0), "height")?;
        let tx_pos = usize_from_value(params.get(1), "tx_pos")?;
        let want_merkle = bool_from_value_or(params.get(2), "merkle", false)?;

        let (txid, merkle) = get_id_from_pos(self.query.chain(), height, tx_pos, want_merkle)?;

        if !want_merkle {
            return Ok(json!(txid.to_hex()));
        }

        let merkle_vec: Vec<String> = merkle.into_iter().map(|entry| entry.to_hex()).collect();

        Ok(json!({
            "tx_hash" : txid.to_hex(),
            "merkle" : merkle_vec}))
    }

    fn handle_command(&mut self, method: &str, params: &[Value], id: &Value) -> Result<Value> {
        let timer = self
            .stats
            .latency
            .with_label_values(&[method])
            .start_timer();
        let result = match method {
            "blockchain.block.header" => self.blockchain_block_header(&params),
            "blockchain.block.headers" => self.blockchain_block_headers(&params),
            "blockchain.estimatefee" => self.blockchain_estimatefee(&params),
            "blockchain.headers.subscribe" => self.blockchain_headers_subscribe(),
            "blockchain.relayfee" => self.blockchain_relayfee(),
            #[cfg(not(feature = "liquid"))]
            "blockchain.scripthash.get_balance" => self.blockchain_scripthash_get_balance(&params),
            "blockchain.scripthash.get_history" => self.blockchain_scripthash_get_history(&params),
            "blockchain.scripthash.listunspent" => self.blockchain_scripthash_listunspent(&params),
            "blockchain.scripthash.subscribe" => self.blockchain_scripthash_subscribe(&params),
            "blockchain.transaction.broadcast" => self.blockchain_transaction_broadcast(&params),
            "blockchain.transaction.get" => self.blockchain_transaction_get(&params),
            "blockchain.transaction.get_merkle" => self.blockchain_transaction_get_merkle(&params),
            "blockchain.transaction.id_from_pos" => {
                self.blockchain_transaction_id_from_pos(&params)
            }
            "mempool.get_fee_histogram" => self.mempool_get_fee_histogram(),
            "server.banner" => self.server_banner(),
            "server.donation_address" => self.server_donation_address(),
            "server.peers.subscribe" => self.server_peers_subscribe(),
            "server.add_peer" => self.server_add_peer(),
            "server.ping" => Ok(Value::Null),
            "server.version" => self.server_version(),
            "server.features" => self.server_features(),
            &_ => bail!("unknown method {} {:?}", method, params),
        };
        timer.observe_duration();
        // TODO: return application errors should be sent to the client
        Ok(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => {
                warn!(
                    "rpc #{} {} {:?} failed: {}",
                    id,
                    method,
                    params,
                    e.display_chain()
                );
                json!({"jsonrpc": "2.0", "id": id, "error": format!("{}", e)})
            }
        })
    }

    fn update_subscriptions(&mut self) -> Result<Vec<Value>> {
        let timer = self
            .stats
            .latency
            .with_label_values(&["periodic_update"])
            .start_timer();
        let mut result = vec![];
        if let Some(ref mut last_entry) = self.last_header_entry {
            let entry = self.query.chain().best_header();
            if *last_entry != entry {
                *last_entry = entry;
                let hex_header = hex::encode(serialize(last_entry.header()));
                let header = json!({"hex": hex_header, "height": last_entry.height()});
                result.push(json!({
                    "jsonrpc": "2.0",
                    "method": "blockchain.headers.subscribe",
                    "params": [header]}));
            }
        }
        for (script_hash, status_hash) in self.status_hashes.iter_mut() {
            let history_txids = get_history(&self.query, &script_hash[..], self.txs_limit)?;
            let new_status_hash = get_status_hash(history_txids)
                .map_or(Value::Null, |h| json!(hex::encode(full_hash(&h[..]))));
            if new_status_hash == *status_hash {
                continue;
            }
            result.push(json!({
                "jsonrpc": "2.0",
                "method": "blockchain.scripthash.subscribe",
                "params": [script_hash.to_hex(), new_status_hash]}));
            *status_hash = new_status_hash;
        }
        timer.observe_duration();
        self.stats
            .subscriptions
            .set(self.status_hashes.len() as i64);
        Ok(result)
    }

    fn send_values(&mut self, values: &[Value]) -> Result<()> {
        for value in values {
            let line = value.to_string() + "\n";
            self.stream
                .write_all(line.as_bytes())
                .chain_err(|| format!("failed to send {}", value))?;
        }
        Ok(())
    }

    fn handle_replies(&mut self) -> Result<()> {
        let empty_params = json!([]);
        loop {
            let msg = self.chan.receiver().recv().chain_err(|| "channel closed")?;
            trace!("RPC {:?}", msg);
            match msg {
                Message::Request(line) => {
                    let cmd: Value = from_str(&line).chain_err(|| "invalid JSON format")?;
                    let reply = match (
                        cmd.get("method"),
                        cmd.get("params").unwrap_or_else(|| &empty_params),
                        cmd.get("id"),
                    ) {
                        (
                            Some(&Value::String(ref method)),
                            &Value::Array(ref params),
                            Some(ref id),
                        ) => self.handle_command(method, params, id)?,
                        _ => bail!("invalid command: {}", cmd),
                    };
                    self.send_values(&[reply])?
                }
                Message::PeriodicUpdate => {
                    let values = self
                        .update_subscriptions()
                        .chain_err(|| "failed to update subscriptions")?;
                    self.send_values(&values)?
                }
                Message::Done => return Ok(()),
            }
        }
    }

    fn handle_requests(mut reader: BufReader<TcpStream>, tx: SyncSender<Message>) -> Result<()> {
        loop {
            let mut line = Vec::<u8>::new();
            reader
                .read_until(b'\n', &mut line)
                .chain_err(|| "failed to read a request")?;
            if line.is_empty() {
                tx.send(Message::Done).chain_err(|| "channel closed")?;
                return Ok(());
            } else {
                if line.starts_with(&[22, 3, 1]) {
                    // (very) naive SSL handshake detection
                    let _ = tx.send(Message::Done);
                    bail!("invalid request - maybe SSL-encrypted data?: {:?}", line)
                }
                match String::from_utf8(line) {
                    Ok(req) => tx
                        .send(Message::Request(req))
                        .chain_err(|| "channel closed")?,
                    Err(err) => {
                        let _ = tx.send(Message::Done);
                        bail!("invalid UTF8: {}", err)
                    }
                }
            }
        }
    }

    pub fn run(mut self) {
        let reader = BufReader::new(self.stream.try_clone().expect("failed to clone TcpStream"));
        let tx = self.chan.sender();
        let child = spawn_thread("reader", || Connection::handle_requests(reader, tx));
        if let Err(e) = self.handle_replies() {
            error!(
                "[{}] connection handling failed: {}",
                self.addr,
                e.display_chain().to_string()
            );
        }
        debug!("[{}] shutting down connection", self.addr);
        let _ = self.stream.shutdown(Shutdown::Both);
        if let Err(err) = child.join().expect("receiver panicked") {
            error!("[{}] receiver failed: {}", self.addr, err);
        }
    }
}

fn get_history(
    query: &Query,
    scripthash: &[u8],
    txs_limit: usize,
) -> Result<Vec<(Txid, Option<BlockId>)>> {
    // to avoid silently trunacting history entries, ask for one extra more than the limit and fail if it exists
    let history_txids = query.history_txids(scripthash, txs_limit + 1);
    ensure!(history_txids.len() <= txs_limit, ErrorKind::TooPopular);
    Ok(history_txids)
}

#[derive(Debug)]
pub enum Message {
    Request(String),
    PeriodicUpdate,
    Done,
}

pub enum Notification {
    Periodic,
    Exit,
}

pub struct RPC {
    notification: Sender<Notification>,
    server: Option<thread::JoinHandle<()>>, // so we can join the server while dropping this ojbect
}

struct Stats {
    latency: HistogramVec,
    subscriptions: Gauge,
}

impl RPC {
    fn start_notifier(
        notification: Channel<Notification>,
        senders: Arc<Mutex<HashMap<i32, SyncSender<Message>>>>,
        acceptor: Sender<Option<(TcpStream, SocketAddr)>>,
    ) {
        spawn_thread("notification", move || {
            for msg in notification.receiver().iter() {
                let senders = senders.lock().unwrap();
                match msg {
                    Notification::Periodic => {
                        for (i, sender) in senders.iter() {
                            if let Err(e) = sender.try_send(Message::PeriodicUpdate) {
                                debug!("failed to send PeriodicUpdate to peer {}: {}", i, e);
                            }
                        }
                    }
                    Notification::Exit => acceptor.send(None).unwrap(),
                }
            }
        });
    }

    fn start_acceptor(addr: SocketAddr) -> Channel<Option<(TcpStream, SocketAddr)>> {
        let chan = Channel::unbounded();
        let acceptor = chan.sender();
        spawn_thread("acceptor", move || {
            let listener =
                TcpListener::bind(addr).unwrap_or_else(|_| panic!("bind({}) failed", addr));
            info!("Electrum RPC server running on {}", addr);
            loop {
                let (stream, addr) = listener.accept().expect("accept failed");
                stream
                    .set_nonblocking(false)
                    .expect("failed to set connection as blocking");
                acceptor.send(Some((stream, addr))).expect("send failed");
            }
        });
        chan
    }

    pub fn start(config: Arc<Config>, query: Arc<Query>, metrics: &Metrics) -> RPC {
        let stats = Arc::new(Stats {
            latency: metrics.histogram_vec(
                HistogramOpts::new("electrum_rpc", "Electrum RPC latency (seconds)"),
                &["method"],
            ),
            subscriptions: metrics.gauge(MetricOpts::new(
                "electrum_subscriptions",
                "# of Electrum subscriptions",
            )),
        });
        let notification = Channel::unbounded();

        let rpc_addr = config.electrum_rpc_addr;
        let txs_limit = config.electrum_txs_limit;

        RPC {
            notification: notification.sender(),
            server: Some(spawn_thread("rpc", move || {
                let senders = Arc::new(Mutex::new(HashMap::<i32, SyncSender<Message>>::new()));
                let handles = Arc::new(Mutex::new(
                    HashMap::<i32, std::thread::JoinHandle<()>>::new(),
                ));

                let acceptor = RPC::start_acceptor(rpc_addr);
                RPC::start_notifier(notification, senders.clone(), acceptor.sender());

                let mut handle_count = 0;
                while let Some((stream, addr)) = acceptor.receiver().recv().unwrap() {
                    let handle_id = handle_count;
                    handle_count += 1;

                    // explicitely scope the shadowed variables for the new thread
                    let handle: thread::JoinHandle<()> = {
                        let query = Arc::clone(&query);
                        let senders = Arc::clone(&senders);
                        let stats = Arc::clone(&stats);
                        let handles = Arc::clone(&handles);

                        spawn_thread("peer", move || {
                            info!("[{}] connected peer #{}", addr, handle_id);
                            let conn = Connection::new(query, stream, addr, stats, txs_limit);
                            senders
                                .lock()
                                .unwrap()
                                .insert(handle_id, conn.chan.sender());
                            conn.run();
                            info!("[{}] disconnected peer #{}", addr, handle_id);

                            senders.lock().unwrap().remove(&handle_id);
                            handles.lock().unwrap().remove(&handle_id);
                        })
                    };

                    handles.lock().unwrap().insert(handle_id, handle);
                }
                trace!("closing {} RPC connections", senders.lock().unwrap().len());
                for sender in senders.lock().unwrap().values() {
                    let _ = sender.send(Message::Done);
                }

                trace!(
                    "waiting for {} RPC handling threads",
                    handles.lock().unwrap().len()
                );
                for (_, handle) in handles.lock().unwrap().drain() {
                    if let Err(e) = handle.join() {
                        warn!("failed to join thread: {:?}", e);
                    }
                }
                trace!("RPC connections are closed");
            })),
        }
    }

    pub fn notify(&self) {
        self.notification.send(Notification::Periodic).unwrap();
    }
}

impl Drop for RPC {
    fn drop(&mut self) {
        trace!("stop accepting new RPCs");
        self.notification.send(Notification::Exit).unwrap();
        if let Some(handle) = self.server.take() {
            handle.join().unwrap();
        }
        trace!("RPC server is stopped");
    }
}
