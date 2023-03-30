use anyhow::{Context, Result};
use bitcoin::blockdata::block::Header as BlockHeader;
use bitcoin::{
    consensus::{
        encode::{self, ReadExt, VarInt},
        Decodable,
    },
    hashes::Hash,
    network::{
        address,
        constants::{self, Magic},
        message::{self, CommandString, NetworkMessage},
        message_blockdata::{GetHeadersMessage, Inventory},
        message_network,
    },
    secp256k1::{self, rand::Rng},
    Block, BlockHash, Network,
};
use crossbeam_channel::{bounded, select, Receiver, Sender};

use std::io::{self, ErrorKind, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{
    chain::{Chain, NewHeader},
    config::ELECTRS_VERSION,
    metrics::{default_duration_buckets, default_size_buckets, Histogram, Metrics},
};

enum Request {
    GetNewHeaders(GetHeadersMessage),
    GetBlocks(Vec<Inventory>),
}

impl Request {
    fn get_new_headers(chain: &Chain) -> Request {
        Request::GetNewHeaders(GetHeadersMessage::new(
            chain.locator(),
            BlockHash::all_zeros(),
        ))
    }

    fn get_blocks(blockhashes: &[BlockHash]) -> Request {
        Request::GetBlocks(
            blockhashes
                .iter()
                .map(|blockhash| Inventory::WitnessBlock(*blockhash))
                .collect(),
        )
    }
}

pub(crate) struct Connection {
    req_send: Sender<Request>,
    blocks_recv: Receiver<Block>,
    headers_recv: Receiver<Vec<BlockHeader>>,
    new_block_recv: Receiver<()>,

    blocks_duration: Histogram,
}

impl Connection {
    /// Get new block headers (supporting reorgs).
    /// https://en.bitcoin.it/wiki/Protocol_documentation#getheaders
    /// Defined as `&mut self` to prevent concurrent invocations (https://github.com/romanz/electrs/pull/526#issuecomment-934685515).
    pub(crate) fn get_new_headers(&mut self, chain: &Chain) -> Result<Vec<NewHeader>> {
        self.req_send.send(Request::get_new_headers(chain))?;
        let headers = self
            .headers_recv
            .recv()
            .context("failed to get new headers")?;

        debug!("got {} new headers", headers.len());
        let prev_blockhash = match headers.first() {
            None => return Ok(vec![]),
            Some(first) => first.prev_blockhash,
        };
        let new_heights = match chain.get_block_height(&prev_blockhash) {
            Some(last_height) => (last_height + 1)..,
            None => bail!("missing prev_blockhash: {}", prev_blockhash),
        };
        Ok(headers
            .into_iter()
            .zip(new_heights)
            .map(NewHeader::from)
            .collect())
    }

    /// Request and process the specified blocks (in the specified order).
    /// See https://en.bitcoin.it/wiki/Protocol_documentation#getblocks for details.
    /// Defined as `&mut self` to prevent concurrent invocations (https://github.com/romanz/electrs/pull/526#issuecomment-934685515).
    pub(crate) fn for_blocks<B, F>(&mut self, blockhashes: B, mut func: F) -> Result<()>
    where
        B: IntoIterator<Item = BlockHash>,
        F: FnMut(BlockHash, Block),
    {
        self.blocks_duration.observe_duration("total", || {
            let blockhashes: Vec<BlockHash> = blockhashes.into_iter().collect();
            if blockhashes.is_empty() {
                return Ok(());
            }
            self.blocks_duration.observe_duration("request", || {
                debug!("loading {} blocks", blockhashes.len());
                self.req_send.send(Request::get_blocks(&blockhashes))
            })?;

            for hash in blockhashes {
                let block = self.blocks_duration.observe_duration("response", || {
                    let block = self
                        .blocks_recv
                        .recv()
                        .with_context(|| format!("failed to get block {}", hash))?;
                    ensure!(block.block_hash() == hash, "got unexpected block");
                    Ok(block)
                })?;
                self.blocks_duration
                    .observe_duration("process", || func(hash, block));
            }
            Ok(())
        })
    }

    /// Note: only a single receiver will get the notification (https://github.com/romanz/electrs/pull/526#issuecomment-934687415).
    pub(crate) fn new_block_notification(&self) -> Receiver<()> {
        self.new_block_recv.clone()
    }

    pub(crate) fn connect(
        network: Network,
        address: SocketAddr,
        metrics: &Metrics,
        magic: Magic,
    ) -> Result<Self> {
        let conn = Arc::new(
            TcpStream::connect(address)
                .with_context(|| format!("{} p2p failed to connect: {:?}", network, address))?,
        );

        let (tx_send, tx_recv) = bounded::<NetworkMessage>(1);
        let (rx_send, rx_recv) = bounded::<RawNetworkMessage>(1);

        let send_duration = metrics.histogram_vec(
            "p2p_send_duration",
            "Time spent sending p2p messages (in seconds)",
            "step",
            default_duration_buckets(),
        );
        let recv_duration = metrics.histogram_vec(
            "p2p_recv_duration",
            "Time spent receiving p2p messages (in seconds)",
            "step",
            default_duration_buckets(),
        );
        let parse_duration = metrics.histogram_vec(
            "p2p_parse_duration",
            "Time spent parsing p2p messages (in seconds)",
            "step",
            default_duration_buckets(),
        );
        let recv_size = metrics.histogram_vec(
            "p2p_recv_size",
            "Size of p2p messages read (in bytes)",
            "message",
            default_size_buckets(),
        );
        let blocks_duration = metrics.histogram_vec(
            "p2p_blocks_duration",
            "Time spent getting blocks via p2p protocol (in seconds)",
            "step",
            default_duration_buckets(),
        );

        let stream = Arc::clone(&conn);
        crate::thread::spawn("p2p_send", move || loop {
            use std::net::Shutdown;
            let msg = match send_duration.observe_duration("wait", || tx_recv.recv()) {
                Ok(msg) => msg,
                Err(_) => {
                    // p2p_loop is closed, so tx_send is disconnected
                    debug!("closing p2p_send thread: no more messages to send");
                    // close the stream reader (p2p_recv thread may block on it)
                    if let Err(e) = stream.shutdown(Shutdown::Read) {
                        warn!("failed to shutdown p2p connection: {}", e)
                    }
                    return Ok(());
                }
            };
            send_duration.observe_duration("send", || {
                trace!("send: {:?}", msg);
                let raw_msg = message::RawNetworkMessage {
                    magic,
                    payload: msg,
                };
                (&*stream)
                    .write_all(encode::serialize(&raw_msg).as_slice())
                    .context("p2p failed to send")
            })?;
        });

        let stream = Arc::clone(&conn);
        crate::thread::spawn("p2p_recv", move || loop {
            let start = Instant::now();
            let raw_msg = RawNetworkMessage::consensus_decode(&mut &*stream);
            {
                let duration = duration_to_seconds(start.elapsed());
                let label = format!(
                    "recv_{}",
                    raw_msg
                        .as_ref()
                        .map(|msg| msg.cmd.as_ref())
                        .unwrap_or("err")
                );
                recv_duration.observe(&label, duration);
            }
            let raw_msg = match raw_msg {
                Ok(raw_msg) => {
                    recv_size.observe(raw_msg.cmd.as_ref(), raw_msg.raw.len() as f64);
                    if raw_msg.magic != magic {
                        bail!("unexpected magic {} (instead of {})", raw_msg.magic, magic)
                    }
                    raw_msg
                }
                Err(encode::Error::Io(e)) if e.kind() == ErrorKind::UnexpectedEof => {
                    debug!("closing p2p_recv thread: connection closed");
                    return Ok(());
                }
                Err(e) => bail!("failed to recv a message from peer: {}", e),
            };

            recv_duration.observe_duration("wait", || rx_send.send(raw_msg))?;
        });

        let (req_send, req_recv) = bounded::<Request>(1);
        let (blocks_send, blocks_recv) = bounded::<Block>(10);
        let (headers_send, headers_recv) = bounded::<Vec<BlockHeader>>(1);
        let (new_block_send, new_block_recv) = bounded::<()>(0);
        let (init_send, init_recv) = bounded::<()>(0);

        tx_send.send(build_version_message())?;

        crate::thread::spawn("p2p_loop", move || loop {
            select! {
                recv(rx_recv) -> result => {
                    let raw_msg = match result {
                        Ok(raw_msg) => raw_msg,
                        Err(_) => {  // p2p_recv is closed, so rx_send is disconnected
                            debug!("closing p2p_loop thread: peer has disconnected");
                            return Ok(()); // new_block_send is dropped, causing the server to exit
                        }
                    };

                    let label = format!("parse_{}", raw_msg.cmd.as_ref());
                    let msg = match parse_duration.observe_duration(&label, || raw_msg.parse()) {
                        Ok(msg) => msg,
                        Err(err) => bail!("failed to parse '{}({:?})': {}", raw_msg.cmd, raw_msg.raw, err),
                    };
                    trace!("recv: {:?}", msg);

                    match msg {
                        NetworkMessage::GetHeaders(_) => {
                            tx_send.send(NetworkMessage::Headers(vec![]))?;
                        }
                        NetworkMessage::Version(version) => {
                            debug!("peer version: {:?}", version);
                            tx_send.send(NetworkMessage::Verack)?;
                        }
                        NetworkMessage::Inv(inventory) => {
                            debug!("peer inventory: {:?}", inventory);
                            if inventory.iter().any(|inv| matches!(inv, Inventory::Block(_))) {
                                let _ = new_block_send.try_send(()); // best-effort notification
                            }

                        },
                        NetworkMessage::Ping(nonce) => {
                            tx_send.send(NetworkMessage::Pong(nonce))?; // connection keep-alive
                        }
                        NetworkMessage::Verack => {
                            init_send.send(())?; // peer acknowledged our version
                        }
                        NetworkMessage::Block(block) => blocks_send.send(block)?,
                        NetworkMessage::Headers(headers) => headers_send.send(headers)?,
                        NetworkMessage::Alert(_) => (),  // https://bitcoin.org/en/alert/2016-11-01-alert-retirement
                        NetworkMessage::Addr(_) => (),   // unused
                        msg => warn!("unexpected message: {:?}", msg),
                    }
                }
                recv(req_recv) -> result => {
                    let req = match result {
                        Ok(req) => req,
                        Err(_) => {  // self is dropped, so req_send is disconnected
                            debug!("closing p2p_loop thread: no more requests to handle");
                            return Ok(());
                        }
                    };
                    let msg = match req {
                        Request::GetNewHeaders(msg) => NetworkMessage::GetHeaders(msg),
                        Request::GetBlocks(inv) => NetworkMessage::GetData(inv),
                    };
                    tx_send.send(msg)?;
                }
            }
        });

        init_recv.recv()?; // wait until `verack` is received

        Ok(Connection {
            req_send,
            blocks_recv,
            headers_recv,
            new_block_recv,
            blocks_duration,
        })
    }
}

fn build_version_message() -> NetworkMessage {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time error")
        .as_secs() as i64;

    let services = constants::ServiceFlags::NONE;

    NetworkMessage::Version(message_network::VersionMessage {
        version: constants::PROTOCOL_VERSION,
        services,
        timestamp,
        receiver: address::Address::new(&addr, services),
        sender: address::Address::new(&addr, services),
        nonce: secp256k1::rand::thread_rng().gen(),
        user_agent: format!("/electrs:{}/", ELECTRS_VERSION),
        start_height: 0,
        relay: false,
    })
}

struct RawNetworkMessage {
    magic: Magic,
    cmd: CommandString,
    raw: Vec<u8>,
}

impl RawNetworkMessage {
    fn parse(&self) -> Result<NetworkMessage> {
        let mut raw: &[u8] = &self.raw;
        let payload = match self.cmd.as_ref() {
            "version" => NetworkMessage::Version(Decodable::consensus_decode(&mut raw)?),
            "verack" => NetworkMessage::Verack,
            "inv" => NetworkMessage::Inv(Decodable::consensus_decode(&mut raw)?),
            "notfound" => NetworkMessage::NotFound(Decodable::consensus_decode(&mut raw)?),
            "block" => NetworkMessage::Block(Decodable::consensus_decode(&mut raw)?),
            "headers" => {
                let len = VarInt::consensus_decode(&mut raw)?.0;
                let mut headers = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    headers.push(Block::consensus_decode(&mut raw)?.header);
                }
                NetworkMessage::Headers(headers)
            }
            "ping" => NetworkMessage::Ping(Decodable::consensus_decode(&mut raw)?),
            "pong" => NetworkMessage::Pong(Decodable::consensus_decode(&mut raw)?),
            "reject" => NetworkMessage::Reject(Decodable::consensus_decode(&mut raw)?),
            "alert" => NetworkMessage::Alert(Decodable::consensus_decode(&mut raw)?),
            "addr" => NetworkMessage::Addr(Decodable::consensus_decode(&mut raw)?),
            _ => bail!(
                "unsupported message: command={}, payload={:?}",
                self.cmd,
                self.raw
            ),
        };
        Ok(payload)
    }
}

impl Decodable for RawNetworkMessage {
    fn consensus_decode<D: io::Read + ?Sized>(d: &mut D) -> Result<Self, encode::Error> {
        let magic = Decodable::consensus_decode(d)?;
        let cmd = Decodable::consensus_decode(d)?;

        let len = u32::consensus_decode(d)?;
        let _checksum = <[u8; 4]>::consensus_decode(d)?; // assume data is correct
        let mut raw = vec![0u8; len as usize];
        d.read_slice(&mut raw)?;

        Ok(RawNetworkMessage { magic, cmd, raw })
    }
}

/// `duration_to_seconds` converts Duration to seconds.
#[inline]
pub fn duration_to_seconds(d: Duration) -> f64 {
    let nanos = f64::from(d.subsec_nanos()) / 1e9;
    d.as_secs() as f64 + nanos
}
