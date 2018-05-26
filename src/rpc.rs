use bitcoin::blockdata::block::BlockHeader;
use bitcoin::blockdata::transaction::Transaction;
use bitcoin::network::serialize::{deserialize, serialize};
use bitcoin::util::hash::Sha256dHash;
use error_chain::ChainedError;
use hex;
use serde_json::{from_str, Number, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::Duration;

use query::Query;
use util::HeaderEntry;

use errors::*;

// TODO: Sha256dHash should be a generic hash-container (since script hash is single SHA256)
fn hash_from_value(val: Option<&Value>) -> Result<Sha256dHash> {
    let script_hash = val.chain_err(|| "missing hash")?;
    let script_hash = script_hash.as_str().chain_err(|| "non-string hash")?;
    let script_hash = Sha256dHash::from_hex(script_hash).chain_err(|| "non-hex hash")?;
    Ok(script_hash)
}

fn jsonify_header(header: &BlockHeader, height: usize) -> Value {
    json!({
        "block_height": height,
        "version": header.version,
        "prev_block_hash": header.prev_blockhash.be_hex_string(),
        "merkle_root": header.merkle_root.be_hex_string(),
        "timestamp": header.time,
        "bits": header.bits,
        "nonce": header.nonce
    })
}

struct Connection {
    query: Arc<Query>,
    last_header_entry: Option<HeaderEntry>,
    status_hashes: HashMap<Sha256dHash, Value>, // ScriptHash -> StatusHash
    stream: TcpStream,
    addr: SocketAddr,
}

impl Connection {
    pub fn new(query: Arc<Query>, stream: TcpStream, addr: SocketAddr) -> Connection {
        Connection {
            query: query,
            last_header_entry: None, // disable header subscription for now
            status_hashes: HashMap::new(),
            stream,
            addr,
        }
    }

    fn blockchain_headers_subscribe(&mut self) -> Result<Value> {
        let entry = self.query.get_best_header()?;
        self.last_header_entry = Some(entry.clone());
        Ok(jsonify_header(entry.header(), entry.height()))
    }

    fn server_version(&self) -> Result<Value> {
        Ok(json!(["RustElectrum 0.1.0", "1.2"]))
    }

    fn server_banner(&self) -> Result<Value> {
        Ok(json!("Welcome to RustElectrum Server!\n"))
    }

    fn server_donation_address(&self) -> Result<Value> {
        Ok(Value::Null)
    }

    fn server_peers_subscribe(&self) -> Result<Value> {
        Ok(json!([]))
    }

    fn mempool_get_fee_histogram(&self) -> Result<Value> {
        Ok(json!(self.query.get_fee_histogram()))
    }

    fn blockchain_block_get_chunk(&self, params: &[Value]) -> Result<Value> {
        const CHUNK_SIZE: usize = 2016;
        let index = params.get(0).chain_err(|| "missing index")?;
        let index = index.as_u64().chain_err(|| "non-number index")? as usize;
        let heights: Vec<usize> = (0..CHUNK_SIZE).map(|h| index * CHUNK_SIZE + h).collect();
        let headers: Vec<String> = self.query
            .get_headers(&heights)
            .into_iter()
            .map(|x| hex::encode(&serialize(&x).unwrap()))
            .collect();
        Ok(json!(headers.join("")))
    }

    fn blockchain_block_get_header(&self, params: &[Value]) -> Result<Value> {
        let height = params.get(0).chain_err(|| "missing height")?;
        let height = height.as_u64().chain_err(|| "non-number height")? as usize;
        let headers = self.query.get_headers(&vec![height]);
        Ok(json!(jsonify_header(&headers[0], height)))
    }

    fn blockchain_estimatefee(&self, params: &[Value]) -> Result<Value> {
        let blocks = params.get(0).chain_err(|| "missing blocks")?;
        let blocks = blocks.as_u64().chain_err(|| "non-number blocks")? as usize;
        let fee_rate = self.query.estimate_fee(blocks); // in BTC/kB
        Ok(json!(fee_rate))
    }

    fn blockchain_relayfee(&self) -> Result<Value> {
        Ok(json!(0.0)) // allow sending transactions with any fee.
    }

    fn blockchain_scripthash_subscribe(&mut self, params: &[Value]) -> Result<Value> {
        let script_hash = hash_from_value(params.get(0)).chain_err(|| "bad script_hash")?;
        let status = self.query.status(&script_hash[..])?;
        let result = status.hash().map_or(Value::Null, |h| json!(hex::encode(h)));
        self.status_hashes.insert(script_hash, result.clone());
        Ok(result)
    }

    fn blockchain_scripthash_get_balance(&self, params: &[Value]) -> Result<Value> {
        let script_hash = hash_from_value(params.get(0)).chain_err(|| "bad script_hash")?;
        let status = self.query.status(&script_hash[..])?;
        Ok(
            json!({ "confirmed": status.confirmed_balance(), "unconfirmed": status.mempool_balance() }),
        )
    }

    fn blockchain_scripthash_get_history(&self, params: &[Value]) -> Result<Value> {
        let script_hash = hash_from_value(params.get(0)).chain_err(|| "bad script_hash")?;
        let status = self.query.status(&script_hash[..])?;
        Ok(json!(Value::Array(
            status
                .history()
                .into_iter()
                .map(|item| json!({"height": item.0, "tx_hash": item.1.be_hex_string()}))
                .collect()
        )))
    }

    fn blockchain_transaction_broadcast(&self, params: &[Value]) -> Result<Value> {
        let tx = params.get(0).chain_err(|| "missing tx")?;
        let tx = tx.as_str().chain_err(|| "non-string tx")?;
        let tx = hex::decode(&tx).chain_err(|| "non-hex tx")?;
        let tx: Transaction = deserialize(&tx).chain_err(|| "failed to parse tx")?;
        let txid = self.query.broadcast(&tx)?;
        Ok(json!(txid.be_hex_string()))
    }

    fn blockchain_transaction_get(&self, params: &[Value]) -> Result<Value> {
        // TODO: handle 'verbose' param
        let tx_hash = hash_from_value(params.get(0)).chain_err(|| "bad tx_hash")?;
        let tx = self.query.get_tx(&tx_hash)?;
        Ok(json!(hex::encode(&serialize(&tx).unwrap())))
    }

    fn blockchain_transaction_get_merkle(&self, params: &[Value]) -> Result<Value> {
        let tx_hash = hash_from_value(params.get(0)).chain_err(|| "bad tx_hash")?;
        let height = params.get(1).chain_err(|| "missing height")?;
        let height = height.as_u64().chain_err(|| "non-number height")? as usize;
        let (merkle, pos) = self.query
            .get_merkle_proof(&tx_hash, height)
            .chain_err(|| "cannot create merkle proof")?;
        let merkle: Vec<String> = merkle
            .into_iter()
            .map(|txid| txid.be_hex_string())
            .collect();
        Ok(json!({
                "block_height": height,
                "merkle": merkle,
                "pos": pos}))
    }

    fn handle_command(&mut self, method: &str, params: &[Value], id: &Number) -> Result<Value> {
        let result = match method {
            "blockchain.headers.subscribe" => self.blockchain_headers_subscribe(),
            "server.version" => self.server_version(),
            "server.banner" => self.server_banner(),
            "server.donation_address" => self.server_donation_address(),
            "server.peers.subscribe" => self.server_peers_subscribe(),
            "mempool.get_fee_histogram" => self.mempool_get_fee_histogram(),
            "blockchain.block.get_chunk" => self.blockchain_block_get_chunk(&params),
            "blockchain.block.get_header" => self.blockchain_block_get_header(&params),
            "blockchain.estimatefee" => self.blockchain_estimatefee(&params),
            "blockchain.relayfee" => self.blockchain_relayfee(),
            "blockchain.scripthash.subscribe" => self.blockchain_scripthash_subscribe(&params),
            "blockchain.scripthash.get_balance" => self.blockchain_scripthash_get_balance(&params),
            "blockchain.scripthash.get_history" => self.blockchain_scripthash_get_history(&params),
            "blockchain.transaction.broadcast" => self.blockchain_transaction_broadcast(&params),
            "blockchain.transaction.get" => self.blockchain_transaction_get(&params),
            "blockchain.transaction.get_merkle" => self.blockchain_transaction_get_merkle(&params),
            &_ => bail!("unknown method {} {:?}", method, params),
        };
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
        let mut result = vec![];
        if let Some(ref mut last_entry) = self.last_header_entry {
            let entry = self.query.get_best_header()?;
            if *last_entry != entry {
                *last_entry = entry;
                let header = jsonify_header(last_entry.header(), last_entry.height());
                result.push(json!({
                    "jsonrpc": "2.0",
                    "method": "blockchain.headers.subscribe",
                    "params": [header]}));
            }
        }
        for (script_hash, status_hash) in self.status_hashes.iter_mut() {
            let status = self.query.status(&script_hash[..])?;
            let new_status_hash = status.hash().map_or(Value::Null, |h| json!(hex::encode(h)));
            if new_status_hash == *status_hash {
                continue;
            }
            result.push(json!({
                "jsonrpc": "2.0",
                "method": "blockchain.scripthash.subscribe",
                "params": [script_hash.be_hex_string(), new_status_hash]}));
            *status_hash = new_status_hash;
        }
        Ok(result)
    }

    fn send_value(&mut self, v: Value) -> Result<()> {
        debug!("[{}] <- {}", self.addr, v);
        let line = v.to_string() + "\n";
        self.stream
            .write_all(line.as_bytes())
            .chain_err(|| format!("failed to send {}", v))
    }

    fn handle_replies(&mut self, chan: &Channel) -> Result<()> {
        let poll_duration = Duration::from_secs(5);
        let rx = chan.receiver();
        loop {
            let msg = match rx.recv_timeout(poll_duration) {
                Ok(msg) => msg,
                Err(RecvTimeoutError::Timeout) => Message::PeriodicUpdate,
                Err(RecvTimeoutError::Disconnected) => bail!("channel closed"),
            };
            match msg {
                Message::Request(line) => {
                    let cmd: Value = from_str(&line).chain_err(|| "invalid JSON format")?;
                    debug!("[{}] -> {}", self.addr, cmd);
                    let reply = match (cmd.get("method"), cmd.get("params"), cmd.get("id")) {
                        (
                            Some(&Value::String(ref method)),
                            Some(&Value::Array(ref params)),
                            Some(&Value::Number(ref id)),
                        ) => self.handle_command(method, params, id)?,
                        _ => bail!("invalid command: {}", cmd),
                    };
                    self.send_value(reply)?
                }
                Message::PeriodicUpdate => {
                    for update in self.update_subscriptions()
                        .chain_err(|| "failed to update subscriptions")?
                    {
                        self.send_value(update)?
                    }
                }
                Message::Done => {
                    debug!("done");
                    break;
                }
            }
        }
        Ok(())
    }

    fn handle_requests(mut reader: BufReader<TcpStream>, tx: SyncSender<Message>) {
        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)  // TODO: use .lines() iterator
                .expect("failed to read a request");
            if line.is_empty() {
                tx.send(Message::Done).expect("channel closed");
                break;
            } else {
                tx.send(Message::Request(line)).expect("channel closed");
            }
        }
    }

    pub fn run(mut self) {
        let reader = BufReader::new(self.stream.try_clone().expect("failed to clone TcpStream"));
        let chan = Channel::new();
        let tx = chan.sender();
        let child = thread::spawn(|| Connection::handle_requests(reader, tx));
        if let Err(e) = self.handle_replies(&chan) {
            error!(
                "[{}] connection handling failed: {}",
                self.addr,
                e.display_chain().to_string()
            );
        }
        let _ = self.stream.shutdown(Shutdown::Both);
        if child.join().is_err() {
            error!("[{}] receiver panicked", self.addr);
        }
    }
}

pub enum Message {
    Request(String),
    PeriodicUpdate,
    Done,
}

pub struct Channel {
    tx: SyncSender<Message>,
    rx: Receiver<Message>,
}

impl Channel {
    pub fn new() -> Channel {
        let (tx, rx) = sync_channel(10);
        Channel { tx, rx }
    }

    pub fn sender(&self) -> SyncSender<Message> {
        self.tx.clone()
    }

    pub fn receiver(&self) -> &Receiver<Message> {
        &self.rx
    }
}

pub fn start(addr: &SocketAddr, query: Arc<Query>) -> thread::JoinHandle<()> {
    let listener = TcpListener::bind(addr).expect(&format!("bind({}) failed", addr));
    info!("RPC server running on {}", addr);
    thread::spawn(move || loop {
        let (stream, addr) = listener.accept().expect("accept failed");
        info!("[{}] connected peer", addr);
        Connection::new(query.clone(), stream, addr).run();
        info!("[{}] disconnected peer", addr);
    })
}
