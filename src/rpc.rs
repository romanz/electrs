use serde_json::{from_str, Number, Value};

use std::net::{SocketAddr, TcpListener, TcpStream};

use std::io::{BufRead, BufReader, Write};

error_chain!{}

fn blockchain_headers_subscribe() -> Result<Value> {
    Ok(json!({}))
}

fn server_version() -> Result<Value> {
    Ok(json!(["LES 0.1.0", "1.2"]))
}

fn server_banner() -> Result<Value> {
    Ok(json!("Welcome to Local Electrum Server!\n"))
}

fn server_donation_address() -> Result<Value> {
    Ok(json!("No, thanks :)\n"))
}

fn server_peers_subscribe() -> Result<Value> {
    Ok(json!([]))
}

fn mempool_get_fee_histogram() -> Result<Value> {
    Ok(json!([])) // TODO: consult with actual mempool
}

fn blockchain_estimatefee(_params: &[&str]) -> Result<Value> {
    Ok(json!(1e-5)) // TODO: consult with actual mempool
}

fn blockchain_scripthash_subscribe(_params: &[&str]) -> Result<Value> {
    Ok(json!("HEX_STATUS"))
}

fn blockchain_scripthash_get_history(_params: &[&str]) -> Result<Value> {
    Ok(json!([])) // TODO: list of {tx_hash: "ABC", height: 123}
}

fn blockchain_transaction_get(_params: &[&str]) -> Result<Value> {
    Ok(json!("HEX_TX")) // TODO: list of {tx_hash: "ABC", height: 123}
}

fn blockchain_transaction_get_merkle(_params: &[&str]) -> Result<Value> {
    Ok(json!({"block_height": 123, "merkle": ["A", "B", "C"], "pos": 45}))
}

fn handle_command(method: &str, params_values: &[Value], id: &Number) -> Result<Value> {
    let mut params = Vec::<&str>::new();
    for value in params_values {
        if let Some(s) = value.as_str() {
            params.push(s);
        } else {
            bail!("invalid param: {:?}", value);
        }
    }
    let result = match method {
        "blockchain.headers.subscribe" => blockchain_headers_subscribe(),
        "server.version" => server_version(),
        "server.banner" => server_banner(),
        "server.donation_address" => server_donation_address(),
        "server.peers.subscribe" => server_peers_subscribe(),
        "mempool.get_fee_histogram" => mempool_get_fee_histogram(),
        "blockchain.estimatefee" => blockchain_estimatefee(&params),
        "blockchain.scripthash.subscribe" => blockchain_scripthash_subscribe(&params),
        "blockchain.scripthash.get_history" => blockchain_scripthash_get_history(&params),
        "blockchain.transaction.get" => blockchain_transaction_get(&params),
        "blockchain.transaction.get_merkle" => blockchain_transaction_get_merkle(&params),
        &_ => bail!("unknown method {} {:?}", method, params),
    }?;
    let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
    Ok(reply)
}

fn handle_client(mut stream: TcpStream, addr: SocketAddr) -> Result<()> {
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
            ) => handle_command(method, params, id)?,
            _ => bail!("invalid command: {}", cmd),
        };

        info!("[{}] {} -> {}", addr, cmd, reply);
        let mut line = reply.to_string();
        line.push_str("\n");
        stream
            .write_all(line.as_bytes())
            .chain_err(|| "failed to send response")?;
    }
    Ok(())
}

pub fn serve() {
    let listener = TcpListener::bind("127.0.0.1:50001").unwrap();
    loop {
        let (stream, addr) = listener.accept().unwrap();
        info!("[{}] connected peer", addr);
        match handle_client(stream, addr) {
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
