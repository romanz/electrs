use anyhow::{Context, Result};
use crossbeam_channel::{select, unbounded, Sender};
use rayon::prelude::*;

use std::{
    collections::hash_map::HashMap,
    io::{BufRead, BufReader, Write},
    net::{Shutdown, TcpListener, TcpStream},
};

use crate::{
    config::Config,
    electrum::{Client, Rpc},
    signals::ExitError,
    thread::spawn,
};

struct Peer {
    id: usize,
    client: Client,
    stream: TcpStream,
}

impl Peer {
    fn new(id: usize, stream: TcpStream) -> Self {
        let client = Client::default();
        Self { id, client, stream }
    }

    fn send(&mut self, values: Vec<String>) -> Result<()> {
        for mut value in values {
            debug!("{}: send {}", self.id, value);
            value += "\n";
            self.stream
                .write_all(value.as_bytes())
                .with_context(|| format!("failed to send response: {:?}", value))?;
        }
        Ok(())
    }

    fn disconnect(self) {
        if let Err(e) = self.stream.shutdown(Shutdown::Both) {
            warn!("{}: failed to shutdown TCP connection {}", self.id, e)
        }
    }
}

pub fn run() -> Result<()> {
    let result = serve();
    if let Err(e) = &result {
        for cause in e.chain() {
            if cause.downcast_ref::<ExitError>().is_some() {
                info!("electrs stopped: {:?}", e);
                return Ok(());
            }
        }
    }
    result.context("electrs failed")
}

fn serve() -> Result<()> {
    let config = Config::from_args();
    let mut rpc = Rpc::new(&config)?;

    let (server_tx, server_rx) = unbounded();
    if !config.disable_electrum_rpc {
        let listener = TcpListener::bind(config.electrum_rpc_addr)?;
        info!("serving Electrum RPC on {}", listener.local_addr()?);
        spawn("accept_loop", || accept_loop(listener, server_tx)); // detach accepting thread
    };

    let new_block_rx = rpc.new_block_notification();
    let mut peers = HashMap::<usize, Peer>::new();
    loop {
        rpc.sync().context("sync failed")?; // initial sync and compaction may take a few hours
        if config.sync_once {
            return Ok(());
        }
        peers = notify_peers(&rpc, peers); // peers are disconnected on error.
        loop {
            select! {
                // Handle signals for graceful shutdown
                recv(rpc.signal().receiver()) -> result => {
                    result.context("signal channel disconnected")?;
                    rpc.signal().exit_flag().poll().context("RPC server interrupted")?;
                },
                // Handle new blocks' notifications
                recv(new_block_rx) -> result => match result {
                    Ok(_) => break, // sync and update
                    Err(_) => return Ok(()), // daemon is shutting down
                },
                // Handle Electrum RPC requests
                recv(server_rx) -> event => {
                    let event = event.context("server disconnected")?;
                    handle_event(&rpc, &mut peers, event);
                },
                default(config.wait_duration) => break, // sync and update
            };
            // continue RPC processing (if more requests are pending)
            if server_rx.is_empty() {
                break;
            }
        }
    }
}

fn notify_peers(rpc: &Rpc, peers: HashMap<usize, Peer>) -> HashMap<usize, Peer> {
    peers
        .into_par_iter()
        .filter_map(|(_, mut peer)| match notify_peer(rpc, &mut peer) {
            Ok(()) => Some((peer.id, peer)),
            Err(e) => {
                error!("failed to notify peer {}: {}", peer.id, e);
                peer.disconnect();
                None
            }
        })
        .collect()
}

fn notify_peer(rpc: &Rpc, peer: &mut Peer) -> Result<()> {
    let notifications = rpc
        .update_client(&mut peer.client)
        .context("failed to generate notifications")?;
    peer.send(notifications)
        .context("failed to send notifications")
}

struct Event {
    peer_id: usize,
    msg: Message,
}

enum Message {
    New(TcpStream),
    Request(String),
    Done,
}

fn handle_event(rpc: &Rpc, peers: &mut HashMap<usize, Peer>, event: Event) {
    let Event { msg, peer_id } = event;
    match msg {
        Message::New(stream) => {
            debug!("{}: connected", peer_id);
            peers.insert(peer_id, Peer::new(peer_id, stream));
        }
        Message::Request(line) => {
            let result = match peers.get_mut(&peer_id) {
                Some(peer) => handle_request(rpc, peer, &line),
                None => return, // unknown peer
            };
            if let Err(e) = result {
                error!("{}: disconnecting due to {}", peer_id, e);
                peers.remove(&peer_id).unwrap().disconnect();
            }
        }
        Message::Done => {
            // already disconnected, just remove from peers' map
            peers.remove(&peer_id);
        }
    }
}

fn handle_request(rpc: &Rpc, peer: &mut Peer, line: &str) -> Result<()> {
    let response = rpc.handle_request(&mut peer.client, line);
    peer.send(vec![response])
}

fn accept_loop(listener: TcpListener, server_tx: Sender<Event>) -> Result<()> {
    for (peer_id, conn) in listener.incoming().enumerate() {
        let stream = conn.context("failed to accept")?;
        let tx = server_tx.clone();
        spawn("recv_loop", move || {
            let result = recv_loop(peer_id, &stream, tx);
            if let Err(e) = stream.shutdown(Shutdown::Read) {
                warn!("{}: failed to shutdown TCP receiving {}", peer_id, e)
            }
            result
        });
    }
    Ok(())
}

fn recv_loop(peer_id: usize, stream: &TcpStream, server_tx: Sender<Event>) -> Result<()> {
    let msg = Message::New(stream.try_clone()?);
    server_tx.send(Event { peer_id, msg })?;

    for line in BufReader::new(stream).lines() {
        let line = line.with_context(|| format!("{}: recv failed", peer_id))?;
        debug!("{}: recv {}", peer_id, line);
        let msg = Message::Request(line);
        server_tx.send(Event { peer_id, msg })?;
    }

    debug!("{}: disconnected", peer_id);
    let msg = Message::Done;
    server_tx.send(Event { peer_id, msg })?;
    Ok(())
}
