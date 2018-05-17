use bitcoin::blockdata::block::BlockHeader;
use bitcoin::network::serialize::serialize;
use bitcoin::util::hash::Sha256dHash;
use crossbeam;
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use hex;
use itertools;
use serde_json::{from_str, Number, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};

use query::{Query, Status};
use types::FullHash;

error_chain!{}

// TODO: Sha256dHash should be a generic hash-container (since script hash is single SHA256)
fn hash_from_value(val: Option<&Value>) -> Result<Sha256dHash> {
    let script_hash = val.chain_err(|| "missing hash")?;
    let script_hash = script_hash.as_str().chain_err(|| "non-string hash")?;
    let script_hash = Sha256dHash::from_hex(script_hash).chain_err(|| "non-hex hash")?;
    Ok(script_hash)
}

fn history_from_status(status: &Status) -> Vec<(i32, Sha256dHash)> {
    let mut txns_map = HashMap::<Sha256dHash, i32>::new();
    for f in &status.funding {
        txns_map.insert(f.txn_id, f.height);
    }
    for s in &status.spending {
        txns_map.insert(s.txn_id, s.height);
    }
    let mut txns: Vec<(i32, Sha256dHash)> =
        txns_map.into_iter().map(|item| (item.1, item.0)).collect();
    txns.sort();
    txns
}

fn hash_from_status(status: &Status) -> Value {
    let txns = history_from_status(status);
    if txns.is_empty() {
        return Value::Null;
    }

    let mut hash = FullHash::default();
    let mut sha2 = Sha256::new();
    for (height, txn_id) in txns {
        let part = format!("{}:{}:", txn_id.be_hex_string(), height);
        sha2.input(part.as_bytes());
    }
    sha2.result(&mut hash);
    Value::String(hex::encode(&hash))
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

struct Handler<'a> {
    query: &'a Query<'a>,
    headers_subscribe: bool,
    status_hashes: HashMap<Sha256dHash, Value>, // ScriptHash -> StatusHash
}

impl<'a> Handler<'a> {
    pub fn new(query: &'a Query) -> Handler<'a> {
        Handler {
            query: query,
            headers_subscribe: false,
            status_hashes: HashMap::new(),
        }
    }

    fn blockchain_headers_subscribe(&mut self) -> Result<Value> {
        let entry = self.query
            .get_best_header()
            .chain_err(|| "no headers found")?;
        self.headers_subscribe = true;
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
        let headers = self.query.get_headers(&heights);
        let result = itertools::join(
            headers
                .into_iter()
                .map(|x| hex::encode(&serialize(&x).unwrap())),
            "",
        );
        Ok(json!(result))
    }

    fn blockchain_block_get_header(&self, params: &[Value]) -> Result<Value> {
        let height = params.get(0).chain_err(|| "missing height")?;
        let height = height.as_u64().chain_err(|| "non-number height")? as usize;
        let headers = self.query.get_headers(&vec![height]);
        Ok(json!(jsonify_header(&headers[0], height)))
    }

    fn blockchain_estimatefee(&self, _params: &[Value]) -> Result<Value> {
        Ok(json!(-1)) // see mempool_get_fee_histogram() instead.
    }

    fn blockchain_relayfee(&self) -> Result<Value> {
        Ok(json!(0.0)) // allow sending transactions with any fee.
    }

    fn blockchain_scripthash_subscribe(&mut self, params: &[Value]) -> Result<Value> {
        let script_hash = hash_from_value(params.get(0)).chain_err(|| "bad script_hash")?;
        let status = self.query.status(&script_hash[..]);
        let result = hash_from_status(&status);
        self.status_hashes.insert(script_hash, result.clone());
        Ok(result)
    }

    fn blockchain_scripthash_get_balance(&self, params: &[Value]) -> Result<Value> {
        let script_hash = hash_from_value(params.get(0)).chain_err(|| "bad script_hash")?;
        let status = self.query.status(&script_hash[..]);
        Ok(json!({ "confirmed": status.balance })) // TODO: "unconfirmed"
    }

    fn blockchain_scripthash_get_history(&self, params: &[Value]) -> Result<Value> {
        let script_hash = hash_from_value(params.get(0)).chain_err(|| "bad script_hash")?;
        let status = self.query.status(&script_hash[..]);
        Ok(json!(Value::Array(
            history_from_status(&status)
                .into_iter()
                .map(|item| json!({"height": item.0, "tx_hash": item.1.be_hex_string()}))
                .collect()
        )))
    }

    fn blockchain_transaction_get(&self, params: &[Value]) -> Result<Value> {
        // TODO: handle 'verbose' param
        let tx_hash = hash_from_value(params.get(0)).chain_err(|| "bad tx_hash")?;
        let tx = self.query.get_tx(&tx_hash);
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
            "blockchain.transaction.get" => self.blockchain_transaction_get(&params),
            "blockchain.transaction.get_merkle" => self.blockchain_transaction_get_merkle(&params),
            &_ => bail!("unknown method {} {:?}", method, params),
        }?;
        Ok(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    fn update_subscriptions(&mut self) -> Result<Vec<Value>> {
        let mut result = vec![];
        if self.headers_subscribe {
            let entry = self.query
                .get_best_header()
                .chain_err(|| "no headers found")?;
            let header = jsonify_header(entry.header(), entry.height());
            result.push(json!({
                "jsonrpc": "2.0",
                "method": "blockchain.headers.subscribe",
                "params": [header]}));
        }
        for (script_hash, status_hash) in self.status_hashes.iter_mut() {
            let status = self.query.status(&script_hash[..]);
            let new_status_hash = hash_from_status(&status);
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

    fn handle_replies(
        &mut self,
        stream: &mut TcpStream,
        addr: SocketAddr,
        chan: &Channel,
    ) -> Result<()> {
        let rx = chan.receiver();
        loop {
            let msg = rx.recv().chain_err(|| "channel closed")?;
            match msg {
                Message::Request(line) => {
                    let cmd: Value = from_str(&line).chain_err(|| "invalid JSON format")?;
                    debug!("[{}] -> {}", addr, cmd);
                    let reply = match (cmd.get("method"), cmd.get("params"), cmd.get("id")) {
                        (
                            Some(&Value::String(ref method)),
                            Some(&Value::Array(ref params)),
                            Some(&Value::Number(ref id)),
                        ) => self.handle_command(method, params, id)?,
                        _ => bail!("invalid command: {}", cmd),
                    };
                    debug!("[{}] <- {}", addr, reply);
                    let line = reply.to_string() + "\n";
                    stream
                        .write_all(line.as_bytes())
                        .chain_err(|| "failed to send response")?;
                }
                Message::Block(blockhash) => {
                    debug!("blockhash found: {}", blockhash);
                    for update in self.update_subscriptions()
                        .chain_err(|| "failed to get updates")?
                    {
                        debug!("update: {}", update);
                        let line = update.to_string() + "\n";
                        stream
                            .write_all(line.as_bytes())
                            .chain_err(|| "failed to send update")?;
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

    pub fn run(mut self, mut stream: TcpStream, addr: SocketAddr, chan: &Channel) {
        let reader = BufReader::new(stream.try_clone().expect("failed to clone TcpStream"));
        // TODO: figure out graceful shutting down and error logging.
        crossbeam::scope(|scope| {
            let tx = chan.sender();
            scope.spawn(|| Handler::handle_requests(reader, tx));
            self.handle_replies(&mut stream, addr, chan).unwrap();
        });
    }
}

pub enum Message {
    Request(String),
    Block(Sha256dHash),
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

pub fn serve(addr: &str, query: &Query, chan: Channel) {
    let listener = TcpListener::bind(addr).unwrap();
    info!("RPC server running on {}", addr);
    loop {
        let (stream, addr) = listener.accept().unwrap();
        info!("[{}] connected peer", addr);
        Handler::new(query).run(stream, addr, &chan);
        info!("[{}] disconnected peer", addr);
    }
}
