use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use crossbeam_channel::{Receiver, Sender};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    join,
    net::TcpStream,
    task,
};
use tower_http::cors::CorsLayer;

use crate::config::Config;

// Transactions
async fn get_tx() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_tx_status() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_tx_hex() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_tx_raw() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_tx_merkleblock_proof() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_tx_merkle_proof() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_tx_outspend_and_vout() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_tx_outspends() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn post_tx() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

// Addresses
async fn get_address() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_scripthash() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_address_txs() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_scripthash_txs() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_confirmed_txs_by_address() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_last_seen_txs_by_address() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_confirmed_txs_by_scripthash() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_last_seen_txs_by_scripthash() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_unconfirmed_txs_by_address() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_unconfirmed_txs_by_scripthash() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_utxos_by_address() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_utxos_by_scripthash() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_addresses_by_prefix() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

// Blocks
async fn get_block() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_block_header() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_block_status() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_block_txs() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_block_txs_by_index() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_block_txids() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_block_tx() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_block_raw() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_block_height_hash() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_blocks() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_blocks_by_height() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

#[axum::debug_handler]
async fn get_block_height(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, AppError> {
    let req = rpc_request(state.clone(), "blockchain.headers.subscribe", vec![]).await?;
    let res: BlockchainHeadersSubscribe = serde_json::from_slice(&req)?;

    Ok((StatusCode::OK, res.block_height.to_string()))
}

async fn get_block_tip_hash() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

// Mempool
async fn get_mempool() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_mempool_txids() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

async fn get_mempool_recent() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

// Fee estimates
async fn get_fee_estimates() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct BlockchainHeadersSubscribe {
    nonce: u64,
    prev_block_hash: String,
    timestamp: u64,
    merkle_root: String,
    block_height: u32,
    utxo_root: String,
    version: u32,
    bits: u64,
}

#[derive(Serialize)]
struct Request {
    id: Value,
    method: String,
    params: Value,
}

#[derive(Deserialize)]
struct RequestId {
    id: usize,
}

async fn rpc_request(state: Arc<AppState>, method: &str, params: Vec<Value>) -> Result<Vec<u8>> {
    let req_id = state.id.fetch_add(1, Ordering::SeqCst);

    let request = Request {
        id: Value::from(req_id),
        method: method.to_owned(),
        params: Value::from(params),
    };

    let req = serde_json::to_vec(&request)?;

    state.req_tx.send(req)?;
    debug!("hello C");

    loop {
        let (res_id, res) = state.res_rx.recv()?;

        debug!("hello D");

        if req_id == res_id {
            return Ok(res);
        }
    }
}

struct AppState {
    id: AtomicUsize,
    req_tx: Sender<Vec<u8>>,
    res_rx: Receiver<(usize, Vec<u8>)>,
}

/// Esplora HTTP API according to these docs:
/// <https://github.com/Blockstream/esplora/blob/master/API.md#esplora-http-api>
pub async fn serve(config: Arc<Config>) -> Result<()> {
    let socket = TcpStream::connect(config.daemon_rpc_addr).await?;
    let (mut tcp_rx, mut tcp_tx) = socket.into_split();
    let id = AtomicUsize::new(0);

    let (req_tx, req_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
    let (res_tx, res_rx) = crossbeam_channel::unbounded::<(usize, Vec<u8>)>();

    let state = Arc::new(AppState { id, req_tx, res_rx });
    let headers_sub_state = state.clone();

    // TCP writer
    let tcp_writer = task::spawn(async move {
        debug!("writer 1");
        loop {
            let req = req_rx.recv().unwrap();
            debug!("HTTP Request: {}", String::from_utf8_lossy(&req));
            tcp_tx.write_all(&req).await.unwrap();
            debug!("writer 3");
        }
    });

    // TCP reader
    let tcp_reader = task::spawn(async move {
        let mut buf = [0; 1500];
        let mut brackets = 0;
        let mut msg: Vec<u8> = vec![];

        debug!("reader 1");
        while let Ok(_bytes) = tcp_rx.read(&mut buf).await {
            debug!("reader 2");
            for byte in buf {
                if byte == b'{' {
                    debug!("reader {{");
                    brackets += 1;
                } else if byte == b'}' {
                    debug!("reader }}");
                    if brackets == 1 {
                        match serde_json::from_slice(&msg) {
                            Ok(res) => {
                                let RequestId { id } = res;
                                res_tx.send((id, msg)).unwrap();
                                msg = vec![];
                            }
                            Err(e) => {
                                error!("Error parsing message: {e}");
                                debug!("Message was: {}", String::from_utf8_lossy(&msg));
                            }
                        }
                    }
                    brackets -= 1;
                }
                msg.push(byte);
            }
        }

        debug!("reader msg: {}", String::from_utf8_lossy(&msg));
    });

    let subscriber = task::spawn(async move {
        rpc_request(headers_sub_state, "blockchain.headers.subscribe", vec![])
            .await
            .unwrap();
    });

    let server = task::spawn(async move {
        let app = Router::new()
            // Transactions
            .route("/tx/:txid", get(get_tx))
            .route("/tx/:txid/status", get(get_tx_status))
            .route("/tx/:txid/hex", get(get_tx_hex))
            .route("/tx/:txid/raw", get(get_tx_raw))
            .route("/tx/:txid/merkleblock-proof", get(get_tx_merkleblock_proof))
            .route("/tx/:txid/merkle-proof", get(get_tx_merkle_proof))
            .route("/tx/:txid/outspend/:vout", get(get_tx_outspend_and_vout))
            .route("/tx/:txid/outspends", get(get_tx_outspends))
            .route("/tx", post(post_tx))
            // Addresses
            .route("/address/:address", get(get_address))
            .route("/scripthash/:hash", get(get_scripthash))
            .route("/address/:address/txs", get(get_address_txs))
            .route("/scripthash/:hash/txs", get(get_scripthash_txs))
            .route(
                "/address/:address/txs/chain",
                get(get_confirmed_txs_by_address),
            )
            .route(
                "/address/:address/txs/chain/:last_seen_txid",
                get(get_last_seen_txs_by_address),
            )
            .route(
                "/scripthash/:hash/txs/chain",
                get(get_confirmed_txs_by_scripthash),
            )
            .route(
                "/scripthash/:hash/txs/chain/:last_seen_txid",
                get(get_last_seen_txs_by_scripthash),
            )
            .route(
                "/address/:address/txs/mempool",
                get(get_unconfirmed_txs_by_address),
            )
            .route(
                "/scripthash/:hash/txs/mempool",
                get(get_unconfirmed_txs_by_scripthash),
            )
            .route("/address/:address/utxo", get(get_utxos_by_address))
            .route("/scripthash/:hash/utxo", get(get_utxos_by_scripthash))
            .route("/address-prefix/:prefix", get(get_addresses_by_prefix))
            // Blocks
            .route("/block/:hash", get(get_block))
            .route("/block/:hash/header", get(get_block_header))
            .route("/block/:hash/status", get(get_block_status))
            .route("/block/:hash/txs", get(get_block_txs))
            .route("/block/:hash/txs/:start_index", get(get_block_txs_by_index))
            .route("/block/:hash/txids", get(get_block_txids))
            .route("/block/:hash/txid/:index", get(get_block_tx))
            .route("/block/:hash/raw", get(get_block_raw))
            .route("/block-height/:height", get(get_block_height_hash))
            .route("/blocks", get(get_blocks))
            .route("/blocks/:start_height", get(get_blocks_by_height))
            .route("/blocks/tip/height", get(get_block_height))
            .route("/blocks/tip/hash", get(get_block_tip_hash))
            // Mempool
            .route("/mempool", get(get_mempool))
            .route("/mempool/txids", get(get_mempool_txids))
            .route("/mempool/recent", get(get_mempool_recent))
            // Fee estimates
            .route("/fee-estimates", get(get_fee_estimates))
            .with_state(state)
            .layer(CorsLayer::permissive());

        info!("serving Esplora REST on {}", config.http_addr);

        axum::Server::bind(&config.http_addr)
            .serve(app.into_make_service())
            .await
            .unwrap();
    });

    let _ = join!(tcp_writer, tcp_reader, subscriber, server);

    Ok(())
}

struct AppError(anyhow::Error);

// Tell axum how to convert `AppError` into a response.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}

// This enables using `?` on functions that return `Result<_, anyhow::Error>` to turn them into
// `Result<_, AppError>`. That way you don't need to do that manually.
impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}
