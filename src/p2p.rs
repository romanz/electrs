use anyhow::{Context, Result};
use bitcoin::{
    consensus::{encode, Decodable},
    network::{
        address, constants,
        message::{self, NetworkMessage},
        message_blockdata::{GetHeadersMessage, Inventory},
        message_network,
    },
    secp256k1::{self, rand::Rng},
    Block, BlockHash, BlockHeader, Network,
};
use crossbeam_channel::{bounded, select, Receiver, Sender};

use std::io::{ErrorKind, Read, Write};
use std::iter::FromIterator;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    chain::{Chain, NewHeader},
    metrics::{default_duration_buckets, Histogram, Metrics},
};

enum Request {
    GetNewHeaders(GetHeadersMessage),
    GetBlocks(Vec<Inventory>),
}

impl Request {
    fn get_new_headers(chain: &Chain) -> Request {
        Request::GetNewHeaders(GetHeadersMessage::new(
            chain.locator(),
            BlockHash::default(),
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
        let prev_blockhash = match headers.first().map(|h| h.prev_blockhash) {
            None => return Ok(vec![]),
            Some(prev_blockhash) => prev_blockhash,
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
        let blockhashes = Vec::from_iter(blockhashes);
        if blockhashes.is_empty() {
            return Ok(());
        }
        self.blocks_duration.observe_duration("send", || {
            debug!("loading {} blocks", blockhashes.len());
            self.req_send.send(Request::get_blocks(&blockhashes))
        })?;

        for hash in blockhashes {
            let block = self.blocks_duration.observe_duration("recv", || {
                self.blocks_recv
                    .recv()
                    .with_context(|| format!("failed to get block {}", hash))
            })?;

            self.blocks_duration.observe_duration("process", || {
                ensure!(block.block_hash() == hash, "got unexpected block");
                func(hash, block);
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Note: only a single receiver will get the notification (https://github.com/romanz/electrs/pull/526#issuecomment-934687415).
    pub(crate) fn new_block_notification(&self) -> Receiver<()> {
        self.new_block_recv.clone()
    }

    pub(crate) fn connect(
        network: Network,
        address: SocketAddr,
        metrics: &Metrics,
    ) -> Result<Self> {
        let mut stream = TcpStream::connect(address)
            .with_context(|| format!("{} p2p failed to connect: {:?}", network, address))?;
        let mut reader = StreamReader::new(
            stream.try_clone().context("stream failed to clone")?,
            /*buffer_size*/ Some(1 << 20),
        );
        let (tx_send, tx_recv) = bounded::<NetworkMessage>(1);
        let (rx_send, rx_recv) = bounded::<NetworkMessage>(1);

        let send_duration = metrics.histogram_vec(
            "p2p_send_duration",
            "Time spent sending p2p messages",
            "step",
            default_duration_buckets(),
        );
        let recv_duration = metrics.histogram_vec(
            "p2p_recv_duration",
            "Time spent receiving p2p messages",
            "step",
            default_duration_buckets(),
        );
        let blocks_duration = metrics.histogram_vec(
            "p2p_blocks_duration",
            "Time spent getting blocks via p2p protocol",
            "step",
            default_duration_buckets(),
        );

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
                    magic: network.magic(),
                    payload: msg,
                };
                stream
                    .write_all(encode::serialize(&raw_msg).as_slice())
                    .context("p2p failed to send")
            })?;
        });

        crate::thread::spawn("p2p_recv", move || loop {
            use bitcoin::consensus::encode::Error;

            let raw_msg: message::RawNetworkMessage =
                match recv_duration.observe_duration("recv", || reader.read_next()) {
                    Ok(raw_msg) => raw_msg,
                    Err(Error::Io(e)) if e.kind() == ErrorKind::UnexpectedEof => {
                        debug!("closing p2p_recv thread: connection closed");
                        return Ok(());
                    }
                    Err(e) => bail!("failed to recv a message from peer: {}", e),
                };

            recv_duration.observe_duration("wait", || {
                trace!("recv: {:?}", raw_msg.payload);
                rx_send.send(raw_msg.payload)
            })?;
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
                    let msg = match result {
                        Ok(msg) => msg,
                        Err(_) => {  // p2p_recv is closed, so rx_send is disconnected
                            debug!("closing p2p_loop thread: peer has disconnected");
                            return Ok(());
                        }
                    };
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
                            if inventory.iter().any(is_block_inv) {
                                let _ = new_block_send.try_send(()); // best-effort notification
                            }

                        },
                        NetworkMessage::Ping(nonce) => {
                            tx_send.send(NetworkMessage::Pong(nonce))?; // connection keep-alive
                        }
                        NetworkMessage::Verack => {
                            init_send.send(())?; // peer acknowledged our version
                        }
                        NetworkMessage::Alert(_) | NetworkMessage::Addr(_) => {}
                        NetworkMessage::Block(block) => blocks_send.send(block)?,
                        NetworkMessage::Headers(headers) => headers_send.send(headers)?,
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
        user_agent: String::from("electrs"),
        start_height: 0,
        relay: false,
    })
}

fn is_block_inv(inv: &Inventory) -> bool {
    if let Inventory::Block(_) = inv {
        true
    } else {
        false
    }
}

// Struct used to configure stream reader function
struct StreamReader<R: Read> {
    /// Stream to read from
    pub stream: R,
    /// I/O buffer
    data: Vec<u8>,
    /// Buffer containing unparsed message part
    unparsed: Vec<u8>,
}

impl<R: Read> StreamReader<R> {
    /// Constructs new stream reader for a given input stream `stream` with
    /// optional parameter `buffer_size` determining reading buffer size
    pub fn new(stream: R, buffer_size: Option<usize>) -> StreamReader<R> {
        StreamReader {
            stream,
            data: vec![0u8; buffer_size.unwrap_or(64 * 1024)],
            unparsed: vec![],
        }
    }

    /// Reads stream and parses next message from its current input,
    /// also taking into account previously unparsed partial message (if there was such).
    pub fn read_next<D: Decodable>(&mut self) -> Result<D, encode::Error> {
        loop {
            match encode::deserialize_partial::<D>(&self.unparsed) {
                // In this case we just have an incomplete data, so we need to read more
                Err(encode::Error::Io(ref err)) if err.kind() == ErrorKind::UnexpectedEof => {
                    let count = self.stream.read(&mut self.data)?;
                    if count > 0 {
                        self.unparsed.extend(self.data[0..count].iter());
                    } else {
                        return Err(encode::Error::Io(std::io::Error::from(
                            ErrorKind::UnexpectedEof,
                        )));
                    }
                }
                Err(err) => return Err(err),
                // We have successfully read from the buffer
                Ok((message, index)) => {
                    self.unparsed.drain(..index);
                    return Ok(message);
                }
            }
        }
    }
}
