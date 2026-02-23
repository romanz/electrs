use anyhow::{Context, Result};
use bitcoin::blockdata::block::Header as BlockHeader;
use bitcoin::consensus::deserialize;
use bitcoin::BlockHash;
use crossbeam_channel::{bounded, Receiver};

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::chain::{Chain, NewHeader};
use crate::connection::BlockSource;
use crate::metrics::{default_duration_buckets, Histogram, Metrics};
use crate::types::SerBlock;

const MAX_REORG_DEPTH: usize = 1000;

/// Max headers returned per REST call — matches p2p `getheaders` cap.
const HEADER_BATCH_SIZE: usize = 2000;

/// Number of parallel REST connections for block fetching.
const FETCH_CONNECTIONS: usize = 4;

/// Bounded channel depth between fetcher and consumer in `for_blocks`.
const BLOCK_CHANNEL_DEPTH: usize = 10;

/// Size of a serialized block header in bytes.
const HEADER_SIZE: usize = 80;

/// Block source backed entirely by the Bitcoin Core REST interface (for
/// headers and blocks) and ZMQ (for new-block notifications).
///
/// No RPC, no authentication. Requires `rest=1` and `zmqpubhashblock=tcp://<bind-address>:<bind-port>` in bitcoin.conf.
pub struct RestZmqBlockSource {
    rest_conn: RestConn,
    block_fetchers: Vec<RestConn>,
    new_block_recv: Receiver<()>,
    blocks_duration: Histogram,
}

impl RestZmqBlockSource {
    pub fn connect(rest_addr: SocketAddr, zmq_endpoint: &str, metrics: &Metrics) -> Result<Self> {
        // Verify REST is enabled.
        let mut conn = RestConn::connect(rest_addr)?;
        conn.get("/rest/chaininfo.json")
            .context("REST not reachable — ensure bitcoind is started with rest=1")?;

        let new_block_recv = spawn_zmq_listener(zmq_endpoint)?;

        let blocks_duration = metrics.histogram_vec(
            "rest_zmq_blocks_duration",
            "Time spent getting blocks via REST (in seconds)",
            "step",
            default_duration_buckets(),
        );

        let mut block_fetchers = Vec::with_capacity(FETCH_CONNECTIONS);
        for _ in 0..FETCH_CONNECTIONS {
            block_fetchers.push(RestConn::connect(rest_addr)?);
        }

        info!(
            "REST+ZMQ block source ready (rest={}, zmq={})",
            rest_addr, zmq_endpoint
        );

        Ok(Self {
            rest_conn: conn,
            block_fetchers,
            new_block_recv,
            blocks_duration,
        })
    }

    /// `/rest/chaininfo.json` → (best block hash, height).
    fn chain_info(&mut self) -> Result<(BlockHash, usize)> {
        let body = self.rest_conn.get("/rest/chaininfo.json")?;
        let v: serde_json::Value =
            serde_json::from_slice(&body).context("invalid chaininfo JSON")?;

        let tip: BlockHash = v["bestblockhash"]
            .as_str()
            .context("missing bestblockhash")?
            .parse()
            .context("invalid bestblockhash")?;
        let height = v["blocks"].as_u64().context("missing blocks")? as usize;
        Ok((tip, height))
    }

    /// `/rest/blockhashbyheight/<h>.json` → block hash at height.
    fn block_hash_at_height(&mut self, height: usize) -> Result<BlockHash> {
        let path = format!("/rest/blockhashbyheight/{}.json", height);
        let body = self.rest_conn.get(&path)?;
        let v: serde_json::Value =
            serde_json::from_slice(&body).context("invalid blockhashbyheight JSON")?;

        v["blockhash"]
            .as_str()
            .context("missing blockhash")?
            .parse()
            .context("invalid blockhash")
    }

    /// `/rest/headers/<count>/<hash>.bin` → raw 80-byte headers.
    ///
    /// Returns up to `count` headers starting from **and including** `hash`.
    fn raw_headers(&mut self, hash: &BlockHash, count: usize) -> Result<Vec<BlockHeader>> {
        let path = format!("/rest/headers/{}/{}.bin", count, hash);
        let body = self.rest_conn.get(&path)?;

        if body.len() % HEADER_SIZE != 0 {
            bail!(
                "REST headers: unexpected length {} (not a multiple of {})",
                body.len(),
                HEADER_SIZE
            );
        }

        body.chunks_exact(HEADER_SIZE)
            .map(|chunk| deserialize(chunk).context("invalid header from REST"))
            .collect()
    }

    /// Fetch new headers after our tip in one REST call.
    ///
    /// Gets the hash at local_height+1 via blockhashbyheight, then requests
    /// headers starting from that hash. All returned headers are new.
    /// Validates prev_blockhash continuity to detect races with reorgs.
    fn fetch_headers_after_tip(&mut self, chain: &Chain) -> Result<Vec<NewHeader>> {
        let local_height = chain.height();

        // Get the hash of the first block we don't have.
        let next_hash = self.block_hash_at_height(local_height + 1)?;

        // REST headers include the starting hash, so all returned are new.
        let headers = self.raw_headers(&next_hash, HEADER_BATCH_SIZE)?;

        if headers.is_empty() {
            return Ok(vec![]);
        }

        // Verify the first header connects to our tip.
        if headers[0].prev_blockhash != chain.tip() {
            bail!(
                "REST header discontinuity: header at height {} has prev_blockhash {}, expected tip {}",
                local_height + 1,
                headers[0].prev_blockhash,
                chain.tip(),
            );
        }

        // Verify internal continuity.
        for i in 1..headers.len() {
            let expected = headers[i - 1].block_hash();
            if headers[i].prev_blockhash != expected {
                bail!(
                    "REST header chain broken at index {}: prev_blockhash {} != expected {}",
                    i,
                    headers[i].prev_blockhash,
                    expected,
                );
            }
        }

        debug!(
            "got {} new headers via REST (heights {}..={})",
            headers.len(),
            local_height + 1,
            local_height + headers.len(),
        );

        Ok(headers
            .into_iter()
            .zip((local_height + 1)..)
            .map(NewHeader::from)
            .collect())
    }

    /// Reorg slow path: walk backwards from the remote tip until we find a
    /// hash present in our local chain (the fork point).
    fn walk_backwards_for_headers(
        &mut self,
        chain: &Chain,
        remote_tip: BlockHash,
    ) -> Result<Vec<NewHeader>> {
        let mut headers: Vec<BlockHeader> = Vec::new();
        let mut current = remote_tip;

        loop {
            if headers.len() >= MAX_REORG_DEPTH {
                bail!(
                    "reorg deeper than {} blocks — aborting backwards walk",
                    MAX_REORG_DEPTH
                );
            }

            let fetched = self.raw_headers(&current, 1)?;
            let header = fetched
                .into_iter()
                .next()
                .with_context(|| format!("REST: no header for {}", current))?;

            let prev = header.prev_blockhash;
            headers.push(header);

            if let Some(fork_height) = chain.get_block_height(&prev) {
                headers.reverse();
                debug!(
                    "got {} new headers via REST backwards walk (fork at height {})",
                    headers.len(),
                    fork_height,
                );
                return Ok(headers
                    .into_iter()
                    .zip((fork_height + 1)..)
                    .map(NewHeader::from)
                    .collect());
            }
            current = prev;
        }
    }
}

impl BlockSource for RestZmqBlockSource {
    /// Fetch new headers, capped at HEADER_BATCH_SIZE per call.
    ///
    /// Fast path: verify our tip is on the main chain, fetch headers in one
    /// REST call. Slow path (reorg): walk backwards to find fork point.
    fn get_new_headers(&mut self, chain: &Chain) -> Result<Vec<NewHeader>> {
        let (remote_tip, remote_height) = self.chain_info()?;

        // Already at tip.
        if chain.get_block_height(&remote_tip).is_some() {
            return Ok(vec![]);
        }

        let local_height = chain.height();

        // Fast path: verify our tip is still on the active chain.
        if local_height <= remote_height {
            let hash_at_ours = self.block_hash_at_height(local_height)?;
            if hash_at_ours == chain.tip() {
                return self.fetch_headers_after_tip(chain);
            }
        }

        // Reorg (or remote behind us) — walk backwards.
        self.walk_backwards_for_headers(chain, remote_tip)
    }

    /// Fetch blocks via `/rest/block/<hash>.bin` using multiple parallel
    /// keep-alive connections to eliminate per-block dead time.
    ///
    /// Block hashes are striped across N fetcher threads (each with its own
    /// TCP connection). A reorder buffer ensures blocks arrive at the consumer
    /// in the original requested order.
    fn for_blocks<'a>(
        &'a mut self,
        blockhashes: Vec<BlockHash>,
        mut func: Box<dyn FnMut(BlockHash, SerBlock) + 'a>,
    ) -> Result<()> {
        if blockhashes.is_empty() {
            return Ok(());
        }

        debug!(
            "REST: fetching {} blocks ({} connections)",
            blockhashes.len(),
            FETCH_CONNECTIONS
        );

        let blocks_duration = &self.blocks_duration;
        let block_fetchers = &mut self.block_fetchers;

        blocks_duration.observe_duration("total", || {
            // Each fetcher gets (index, hash) pairs so we can reorder.
            let mut work: Vec<Vec<(usize, BlockHash)>> =
                (0..FETCH_CONNECTIONS).map(|_| Vec::new()).collect();
            for (i, hash) in blockhashes.iter().enumerate() {
                work[i % FETCH_CONNECTIONS].push((i, *hash));
            }

            let (tx, rx) = bounded::<(usize, BlockHash, SerBlock)>(BLOCK_CHANNEL_DEPTH);

            std::thread::scope(|s| {
                // Spawn fetcher threads, each borrowing a persistent connection.
                let fetchers: Vec<_> = work
                    .into_iter()
                    .zip(block_fetchers.iter_mut())
                    .enumerate()
                    .map(|(conn_id, (assignments, conn))| {
                        let tx = tx.clone();
                        s.spawn(move || {
                            let mut err = None;
                            for (idx, hash) in assignments {
                                let path = format!("/rest/block/{}.bin", hash);
                                match conn.get(&path) {
                                    Ok(block) => {
                                        if tx.send((idx, hash, block)).is_err() {
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        err = Some(e.context(format!(
                                            "REST conn {}: block {}",
                                            conn_id, hash
                                        )));
                                        break;
                                    }
                                }
                            }
                            err
                        })
                    })
                    .collect();

                // Drop our copy so channel closes when all fetchers finish.
                drop(tx);

                // Reorder buffer: blocks arrive out of order, consumer needs
                // them in sequence.
                let mut next_idx = 0;
                let mut pending: std::collections::HashMap<usize, (BlockHash, SerBlock)> =
                    std::collections::HashMap::new();

                for (idx, hash, block) in rx {
                    pending.insert(idx, (hash, block));

                    // Flush all consecutive ready blocks.
                    while let Some((h, b)) = pending.remove(&next_idx) {
                        blocks_duration.observe_duration("process", || func(h, b));
                        next_idx += 1;
                    }
                }

                // Check for fetcher errors.
                let mut first_err = None;
                for f in fetchers {
                    let err = f.join().expect("fetcher panicked");
                    if first_err.is_none() {
                        first_err = err;
                    }
                }

                if let Some(e) = first_err {
                    return Err(e);
                }

                assert_eq!(next_idx, blockhashes.len(), "not all blocks were processed");
                Ok(())
            })
        })
    }

    fn new_block_notification(&self) -> Receiver<()> {
        self.new_block_recv.clone()
    }
}

/// Persistent HTTP/1.1 connection to bitcoind REST.
/// Reuses a single TCP stream across requests to avoid per-request overhead.
struct RestConn {
    addr: SocketAddr,
    reader: BufReader<TcpStream>,
}

impl RestConn {
    fn connect(addr: SocketAddr) -> Result<Self> {
        let stream = TcpStream::connect(addr)
            .with_context(|| format!("REST: connect to {} failed", addr))?;
        stream.set_read_timeout(Some(Duration::from_secs(120))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(30))).ok();
        let reader = BufReader::new(stream);
        Ok(Self { addr, reader })
    }

    /// GET a path; auto-reconnects once on failure.
    fn get(&mut self, path: &str) -> Result<Vec<u8>> {
        match self.do_get(path) {
            Ok(body) => Ok(body),
            Err(first_err) => {
                debug!("REST: retrying {} after error: {:#}", path, first_err);
                *self = Self::connect(self.addr)?;
                self.do_get(path)
            }
        }
    }

    fn do_get(&mut self, path: &str) -> Result<Vec<u8>> {
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\nAccept-Encoding: identity\r\n\r\n",
            path, self.addr
        );
        self.reader
            .get_mut()
            .write_all(req.as_bytes())
            .context("REST: write failed")?;

        let mut status = String::new();
        let n = self
            .reader
            .read_line(&mut status)
            .context("REST: read status")?;
        if n == 0 {
            bail!("REST: connection closed (EOF reading status)");
        }
        let status_code = status
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .with_context(|| format!("REST: malformed status line: {}", status.trim()))?;
        let is_ok = status_code == 200;

        let mut content_length: Option<usize> = None;
        let mut chunked = false;
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .context("REST: read header")?;
            if n == 0 {
                bail!("REST: connection closed (EOF reading headers)");
            }
            let t = line.trim();
            if t.is_empty() {
                break;
            }
            let lower = t.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                content_length = v.trim().parse().ok();
            }
            if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
                chunked = true;
            }
        }

        let body = if let Some(len) = content_length {
            let mut body = vec![0u8; len];
            self.reader
                .read_exact(&mut body)
                .context("REST: read body")?;
            body
        } else if chunked {
            self.read_chunked()?
        } else if !is_ok {
            // Unknown body framing on error response — connection is
            // potentially desynced. Force reconnect on next request.
            *self = Self::connect(self.addr)?;
            Vec::new()
        } else {
            bail!("REST: no Content-Length and not chunked");
        };

        if !is_ok {
            bail!("REST: {} → {} {}", path, status_code, status.trim());
        }

        Ok(body)
    }

    fn read_chunked(&mut self) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line)?;
            // Strip chunk extensions (e.g. "1a;foo=bar" → "1a").
            let hex = line.trim().split(';').next().unwrap_or("0");
            let size = usize::from_str_radix(hex, 16).context("bad chunk size")?;
            if size == 0 {
                // Drain trailers: read until empty line.
                loop {
                    let mut trailer = String::new();
                    self.reader.read_line(&mut trailer)?;
                    if trailer.trim().is_empty() {
                        break;
                    }
                }
                break;
            }
            let mut chunk = vec![0u8; size];
            self.reader.read_exact(&mut chunk)?;
            body.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            self.reader.read_exact(&mut crlf)?;
        }
        Ok(body)
    }
}

fn spawn_zmq_listener(endpoint: &str) -> Result<Receiver<()>> {
    let ctx = zmq::Context::new();
    let sub = ctx.socket(zmq::SUB).context("ZMQ: create socket")?;
    sub.connect(endpoint)
        .with_context(|| format!("ZMQ: connect to {}", endpoint))?;
    sub.set_subscribe(b"hashblock").context("ZMQ: subscribe")?;

    info!("subscribed to ZMQ hashblock at {}", endpoint);

    let (tx, rx) = bounded::<()>(0);
    let ep = endpoint.to_owned();

    crate::thread::spawn("zmq_listener", move || loop {
        match sub.recv_bytes(0) {
            Ok(_) => {}
            Err(zmq::Error::ETERM) => {
                debug!("ZMQ terminated");
                return Ok(());
            }
            Err(e) => bail!("ZMQ recv on {}: {}", ep, e),
        }
        while sub.get_rcvmore().unwrap_or(false) {
            let _ = sub.recv_bytes(0);
        }
        let _ = tx.try_send(());
    });

    Ok(rx)
}
