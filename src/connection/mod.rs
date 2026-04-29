use crate::{
    chain::{Chain, NewHeader},
    connection::rpc_zmq::RestZmqBlockSource,
    metrics::Metrics,
    types::SerBlock,
};
use bitcoin::{p2p::Magic, BlockHash};
use crossbeam_channel::Receiver;
use std::net::SocketAddr;

mod p2p;
mod rpc_zmq;

pub trait BlockSource: Send + Sync {
    fn get_new_headers(&mut self, chain: &Chain) -> anyhow::Result<Vec<NewHeader>>;
    fn for_blocks<'a>(
        &'a mut self,
        blockhashes: Vec<BlockHash>,
        func: Box<dyn FnMut(BlockHash, SerBlock) + 'a>,
    ) -> anyhow::Result<()>;
    fn new_block_notification(&self) -> Receiver<()>;
}

pub fn make_p2p_connection(
    address: SocketAddr,
    metrics: &Metrics,
    magic: Magic,
) -> anyhow::Result<Box<dyn BlockSource>> {
    Ok(Box::new(p2p::Connection::connect(address, metrics, magic)?))
}

pub fn make_rpc_zmq_connection(
    rest_addr: SocketAddr,
    zmq_endpoint: &str,
    metrics: &Metrics,
) -> anyhow::Result<Box<dyn BlockSource>> {
    Ok(Box::new(RestZmqBlockSource::connect(
        rest_addr,
        zmq_endpoint,
        metrics,
    )?))
}
