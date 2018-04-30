use bitcoin::util::hash::Sha256dHash;
use serde_json::{from_str, Number, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};

use query::Query;

error_chain!{}

struct Handler<'a> {
    query: &'a Query<'a>,
}

impl<'a> Handler<'a> {
    fn blockchain_headers_subscribe(&self) -> Result<Value> {
        Ok(json!({}))
    }

    fn server_version(&self) -> Result<Value> {
        Ok(json!(["LES 0.1.0", "1.2"]))
    }

    fn server_banner(&self) -> Result<Value> {
        Ok(json!("Welcome to Local Electrum Server!\n"))
    }

    fn server_donation_address(&self) -> Result<Value> {
        Ok(json!("No, thanks :)\n"))
    }

    fn server_peers_subscribe(&self) -> Result<Value> {
        Ok(json!([]))
    }

    fn mempool_get_fee_histogram(&self) -> Result<Value> {
        Ok(json!([])) // TODO: consult with actual mempool
    }

    fn blockchain_estimatefee(&self, _params: &[&str]) -> Result<Value> {
        Ok(json!(1e-5)) // TODO: consult with actual mempool
    }

    fn blockchain_scripthash_subscribe(&self, _params: &[&str]) -> Result<Value> {
        Ok(json!("HEX_STATUS"))
    }

    fn blockchain_scripthash_get_balance(&self, params: &[&str]) -> Result<Value> {
        let script_hash_hex = params.get(0).chain_err(|| "missing parameter")?;
        let script_hash =
            Sha256dHash::from_hex(script_hash_hex).chain_err(|| "invalid scripthash")?;
        let confirmed = self.query.balance(&script_hash[..]);
        Ok(json!({ "confirmed": confirmed })) // TODO: "unconfirmed"
    }

    fn blockchain_scripthash_get_history(&self, _params: &[&str]) -> Result<Value> {
        Ok(json!([])) // TODO: list of {tx_hash: "ABC", height: 123}
    }

    fn blockchain_transaction_get(&self, _params: &[&str]) -> Result<Value> {
        Ok(json!("HEX_TX")) // TODO: list of {tx_hash: "ABC", height: 123}
    }

    fn blockchain_transaction_get_merkle(&self, _params: &[&str]) -> Result<Value> {
        Ok(json!({"block_height": 123, "merkle": ["A", "B", "C"], "pos": 45}))
    }

    fn handle_command(&self, method: &str, params_values: &[Value], id: &Number) -> Result<Value> {
        let mut params = Vec::<&str>::new();
        for value in params_values {
            if let Some(s) = value.as_str() {
                params.push(s);
            } else {
                bail!("invalid param: {:?}", value);
            }
        }
        let result = match method {
            "blockchain.headers.subscribe" => self.blockchain_headers_subscribe(),
            "server.version" => self.server_version(),
            "server.banner" => self.server_banner(),
            "server.donation_address" => self.server_donation_address(),
            "server.peers.subscribe" => self.server_peers_subscribe(),
            "mempool.get_fee_histogram" => self.mempool_get_fee_histogram(),
            "blockchain.estimatefee" => self.blockchain_estimatefee(&params),
            "blockchain.scripthash.subscribe" => self.blockchain_scripthash_subscribe(&params),
            "blockchain.scripthash.get_balance" => self.blockchain_scripthash_get_balance(&params),
            "blockchain.scripthash.get_history" => self.blockchain_scripthash_get_history(&params),
            "blockchain.transaction.get" => self.blockchain_transaction_get(&params),
            "blockchain.transaction.get_merkle" => self.blockchain_transaction_get_merkle(&params),
            &_ => bail!("unknown method {} {:?}", method, params),
        }?;
        let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
        Ok(reply)
    }

    pub fn run(self, mut stream: TcpStream, addr: SocketAddr) -> Result<()> {
        let mut reader = BufReader::new(stream
            .try_clone()
            .chain_err(|| "failed to clone TcpStream")?);
        let mut line = String::new();

        loop {
            line.clear();
            reader
                .read_line(&mut line)
                .chain_err(|| "failed to read a request")?;
            if line.is_empty() {
                break;
            }
            let line = line.trim_right();
            let cmd: Value = from_str(line).chain_err(|| "invalid JSON format")?;

            let reply = match (cmd.get("method"), cmd.get("params"), cmd.get("id")) {
                (
                    Some(&Value::String(ref method)),
                    Some(&Value::Array(ref params)),
                    Some(&Value::Number(ref id)),
                ) => self.handle_command(method, params, id)?,
                _ => bail!("invalid command: {}", cmd),
            };

            debug!("[{}] {} -> {}", addr, cmd, reply);
            let mut line = reply.to_string();
            line.push_str("\n");
            stream
                .write_all(line.as_bytes())
                .chain_err(|| "failed to send response")?;
        }
        Ok(())
    }
}

pub fn serve(addr: &str, query: &Query) {
    let listener = TcpListener::bind(addr).unwrap();
    info!("RPC server running on {}", addr);
    loop {
        let (stream, addr) = listener.accept().unwrap();
        info!("[{}] connected peer", addr);
        let handler = Handler { query };
        match handler.run(stream, addr) {
            Ok(()) => info!("[{}] disconnected peer", addr),
            Err(ref e) => {
                error!("[{}] {}", addr, e);
                for e in e.iter().skip(1) {
                    error!("caused by: {}", e);
                }
            }
        }
    }
}
