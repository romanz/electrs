use anyhow::{Context, Result};
use bitcoin::BlockHash;
use core_rpc::RpcApi;
use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender};
use rayon::prelude::*;

use std::{
    collections::hash_map::HashMap,
    convert::TryFrom,
    io::{BufRead, BufReader, Write},
    net::{Shutdown, TcpListener, TcpStream},
};

use crate::{
    config::Config,
    daemon::rpc_connect,
    electrum::{Client, Rpc},
    signals,
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
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn tip_receiver(config: &Config) -> Result<Receiver<BlockHash>> {
    let duration = u64::try_from(config.wait_duration.as_millis()).unwrap();
    let (tip_tx, tip_rx) = bounded(0);
    let rpc = rpc_connect(config)?;

    use crossbeam_channel::TrySendError;
    spawn("tip_loop", move || loop {
        let tip = rpc.get_best_block_hash()?;
        match tip_tx.try_send(tip) {
            Ok(_) | Err(TrySendError::Full(_)) => (),
            Err(TrySendError::Disconnected(_)) => bail!("tip receiver disconnected"),
        }
        rpc.wait_for_new_block(duration)?;
    });
    Ok(tip_rx)
}

pub fn run(config: &Config, mut rpc: Rpc) -> Result<()> {
    let listener = TcpListener::bind(config.electrum_rpc_addr)?;
    let tip_rx = tip_receiver(config)?;
    info!("serving Electrum RPC on {}", listener.local_addr()?);

    let (server_tx, server_rx) = unbounded();
    spawn("accept_loop", || accept_loop(listener, server_tx)); // detach accepting thread
    let signal_rx = signals::register();

    let mut peers = HashMap::<usize, Peer>::new();
    loop {
        select! {
            recv(signal_rx) -> sig => {
                match sig.context("signal channel disconnected")? {
                    signals::Signal::Exit => break,
                    signals::Signal::Trigger => (),
                }
            },
            recv(tip_rx) -> tip => match tip {
                Ok(_) => (), // sync and update
                Err(_) => break, // daemon is shutting down
            },
            recv(server_rx) -> event => {
                let event = event.context("server disconnected")?;
                let buffered_events = server_rx.iter().take(server_rx.len());
                for event in std::iter::once(event).chain(buffered_events) {
                    handle_event(&rpc, &mut peers, event);
                }
            },
        };
        rpc.sync().context("rpc sync failed")?;
        peers = notify_peers(&rpc, peers); // peers are disconnected on error.
    }
    info!("stopping Electrum RPC server");
    Ok(())
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
            let _ = stream.shutdown(Shutdown::Both);
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
