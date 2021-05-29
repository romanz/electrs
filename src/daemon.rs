use anyhow::{Context, Result};

use std::io::Write;
use std::iter::FromIterator;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use bitcoin::consensus::encode;
use bitcoin::network::stream_reader::StreamReader;
use bitcoin::network::{
    address, constants,
    message::{self, NetworkMessage},
    message_blockdata::{GetHeadersMessage, Inventory},
    message_network,
};
use bitcoin::secp256k1;
use bitcoin::secp256k1::rand::Rng;
use bitcoin::{Amount, Block, BlockHash, Network, Transaction, Txid};
use bitcoincore_rpc::{self, json, RpcApi};

use crate::{
    chain::{Chain, NewHeader},
    config::Config,
};

struct Connection {
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
}

enum PollResult {
    Done(Result<()>),
    Retry,
}

fn rpc_poll(client: &mut bitcoincore_rpc::Client) -> PollResult {
    use bitcoincore_rpc::{
        jsonrpc::error::Error::Rpc as ServerError, Error::JsonRpc as JsonRpcError,
    };

    match client.get_blockchain_info() {
        Ok(info) => {
            let left_blocks = info.headers - info.blocks;
            if info.initial_block_download {
                info!("waiting for IBD to finish: {} blocks left", left_blocks);
                return PollResult::Retry;
            }
            if left_blocks > 0 {
                info!("waiting for {} blocks to download", left_blocks);
                return PollResult::Retry;
            }
            return PollResult::Done(Ok(()));
        }
        Err(err) => {
            if let JsonRpcError(ServerError(ref e)) = err {
                if e.code == -28 {
                    info!("waiting for RPC warmup: {}", e.message);
                    return PollResult::Retry;
                }
            }
            return PollResult::Done(Err(err).context("daemon not available"));
        }
    }
}

pub(crate) fn rpc_connect(config: &Config) -> Result<bitcoincore_rpc::Client> {
    let rpc_url = format!("http://{}", config.daemon_rpc_addr);
    if !config.daemon_cookie_file.exists() {
        bail!("{:?} is missing", config.daemon_cookie_file);
    }
    let rpc_auth = bitcoincore_rpc::Auth::CookieFile(config.daemon_cookie_file.clone());
    let mut client = bitcoincore_rpc::Client::new(rpc_url, rpc_auth)
        .with_context(|| format!("failed to connect to RPC: {}", config.daemon_rpc_addr))?;

    loop {
        match rpc_poll(&mut client) {
            PollResult::Done(result) => return result.map(|()| client),
            PollResult::Retry => {
                std::thread::sleep(std::time::Duration::from_secs(1)); // wait a bit before polling
            }
        }
    }
}

pub struct Daemon {
    p2p: Mutex<Connection>,
    rpc: bitcoincore_rpc::Client,
}

impl Daemon {
    pub fn connect(config: &Config) -> Result<Self> {
        let rpc = rpc_connect(config)?;
        let network_info = rpc.get_network_info()?;
        if network_info.version < 21_00_00 {
            bail!("electrs requires bitcoind 0.21+");
        }
        if !network_info.network_active {
            bail!("electrs requires active bitcoind p2p network");
        }
        let blockchain_info = rpc.get_blockchain_info()?;
        if blockchain_info.pruned {
            bail!("electrs requires non-pruned bitcoind node");
        }
        let p2p = Mutex::new(Connection::connect(config.network, config.daemon_p2p_addr)?);
        Ok(Self { p2p, rpc })
    }

    pub(crate) fn estimate_fee(&self, nblocks: u16) -> Result<Option<Amount>> {
        Ok(self
            .rpc
            .estimate_smart_fee(nblocks, None)
            .context("failed to estimate fee")?
            .fee_rate)
    }

    pub(crate) fn get_relay_fee(&self) -> Result<Amount> {
        Ok(self
            .rpc
            .get_network_info()
            .context("failed to get relay fee")?
            .relay_fee)
    }

    pub(crate) fn broadcast(&self, tx: &Transaction) -> Result<Txid> {
        self.rpc
            .send_raw_transaction(tx)
            .context("failed to broadcast transaction")
    }

    pub(crate) fn get_transaction_info(
        &self,
        txid: &Txid,
        blockhash: Option<BlockHash>,
    ) -> Result<json::GetRawTransactionResult> {
        self.rpc
            .get_raw_transaction_info(txid, blockhash.as_ref())
            .context("failed to get transaction info")
    }

    pub(crate) fn get_transaction_hex(
        &self,
        txid: &Txid,
        blockhash: Option<BlockHash>,
    ) -> Result<json::GetRawTransactionResult> {
        self.rpc
            .get_raw_transaction_info(txid, blockhash.as_ref())
            .context("failed to get transaction info")
    }

    pub(crate) fn get_transaction(
        &self,
        txid: &Txid,
        blockhash: Option<BlockHash>,
    ) -> Result<Transaction> {
        self.rpc
            .get_raw_transaction(txid, blockhash.as_ref())
            .context("failed to get transaction")
    }

    pub(crate) fn get_block_txids(&self, blockhash: BlockHash) -> Result<Vec<Txid>> {
        Ok(self
            .rpc
            .get_block_info(&blockhash)
            .context("failed to get block txids")?
            .tx)
    }

    pub(crate) fn get_mempool_txids(&self) -> Result<Vec<Txid>> {
        self.rpc
            .get_raw_mempool()
            .context("failed to get mempool txids")
    }

    pub(crate) fn get_mempool_entry(&self, txid: &Txid) -> Result<json::GetMempoolEntryResult> {
        self.rpc
            .get_mempool_entry(txid)
            .context("failed to get mempool entry")
    }

    pub(crate) fn get_new_headers(&self, chain: &Chain) -> Result<Vec<NewHeader>> {
        let mut conn = self.p2p.lock().unwrap();

        let msg = GetHeadersMessage::new(chain.locator(), BlockHash::default());
        conn.send(NetworkMessage::GetHeaders(msg))?;
        let headers = match conn.recv().context("failed to get new headers")? {
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

    pub(crate) fn for_blocks<B, F>(&self, blockhashes: B, mut func: F) -> Result<()>
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
        let mut conn = self.p2p.lock().unwrap();
        conn.send(NetworkMessage::GetData(inv))?;
        for hash in blockhashes {
            match conn
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
