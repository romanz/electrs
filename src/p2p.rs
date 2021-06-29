use std::io::Write;
use std::iter::FromIterator;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chain::{Chain, NewHeader};
use anyhow::{Context, Result};
use bitcoin::{
    consensus::encode,
    network::{
        address, constants,
        message::{self, NetworkMessage},
        message_blockdata::{GetHeadersMessage, Inventory},
        message_network,
        stream_reader::StreamReader,
    },
    secp256k1::{self, rand::Rng},
    Block, BlockHash, Network,
};

pub(crate) struct Connection {
    stream: TcpStream,
    reader: StreamReader<TcpStream>,
    network: Network,
}

impl Connection {
    pub fn connect(network: Network, address: SocketAddr) -> Result<Self> {
        let stream = TcpStream::connect(address)
            .with_context(|| format!("{} p2p failed to connect: {:?}", network, address))?;
        let reader = StreamReader::new(
            stream.try_clone().context("stream failed to clone")?,
            /*buffer_size*/ Some(1 << 20),
        );
        let mut conn = Self {
            stream,
            reader,
            network,
        };
        conn.send(build_version_message())?;
        if let NetworkMessage::GetHeaders(_) = conn.recv().context("failed to get headers")? {
            conn.send(NetworkMessage::Headers(vec![]))?;
        }
        Ok(conn)
    }

    fn send(&mut self, msg: NetworkMessage) -> Result<()> {
        trace!("send: {:?}", msg);
        let raw_msg = message::RawNetworkMessage {
            magic: self.network.magic(),
            payload: msg,
        };
        self.stream
            .write_all(encode::serialize(&raw_msg).as_slice())
            .context("p2p failed to send")
    }

    fn recv(&mut self) -> Result<NetworkMessage> {
        loop {
            let raw_msg: message::RawNetworkMessage =
                self.reader.read_next().context("p2p failed to recv")?;

            trace!("recv: {:?}", raw_msg.payload);
            match raw_msg.payload {
                NetworkMessage::Version(version) => {
                    debug!("peer version: {:?}", version);
                    self.send(NetworkMessage::Verack)?;
                }
                NetworkMessage::Ping(nonce) => {
                    self.send(NetworkMessage::Pong(nonce))?;
                }
                NetworkMessage::Verack
                | NetworkMessage::Alert(_)
                | NetworkMessage::Addr(_)
                | NetworkMessage::Inv(_) => {}
                payload => return Ok(payload),
            };
        }
    }

    pub(crate) fn for_blocks<B, F>(&mut self, blockhashes: B, mut func: F) -> Result<()>
    where
        B: IntoIterator<Item = BlockHash>,
        F: FnMut(BlockHash, Block),
    {
        let blockhashes = Vec::from_iter(blockhashes);
        if blockhashes.is_empty() {
            return Ok(());
        }
        let inv = blockhashes
            .iter()
            .map(|h| Inventory::WitnessBlock(*h))
            .collect();
        debug!("loading {} blocks", blockhashes.len());
        self.send(NetworkMessage::GetData(inv))?;
        for hash in blockhashes {
            match self
                .recv()
                .with_context(|| format!("failed to get block {}", hash))?
            {
                NetworkMessage::Block(block) => {
                    assert_eq!(block.block_hash(), hash, "got unexpected block");
                    func(hash, block);
                }
                msg => bail!("unexpected {:?}", msg),
            };
        }
        Ok(())
    }

    pub(crate) fn get_new_headers(&mut self, chain: &Chain) -> Result<Vec<NewHeader>> {
        let msg = GetHeadersMessage::new(chain.locator(), BlockHash::default());
        self.send(NetworkMessage::GetHeaders(msg))?;
        let headers = match self.recv().context("failed to get new headers")? {
            NetworkMessage::Headers(headers) => headers,
            msg => bail!("unexpected {:?}", msg),
        };

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
}

fn build_version_message() -> NetworkMessage {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time error")
        .as_secs() as i64;

    let services = constants::ServiceFlags::NETWORK | constants::ServiceFlags::WITNESS;

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
