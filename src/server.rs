use anyhow::{Context, Result};
use bitcoin::BlockHash;
use bitcoincore_rpc::RpcApi;
use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender};
use rayon::prelude::*;
use serde_json::{de::from_str, Value};

use std::{
    collections::hash_map::HashMap,
    convert::TryFrom,
    io::{BufRead, BufReader, Write},
    net::{Shutdown, TcpListener, TcpStream},
    thread,
};

use crate::{
    config::Config,
    daemon::rpc_connect,
    electrum::{Client, Rpc},
    signals,
};

fn spawn<F>(name: &'static str, f: F) -> thread::JoinHandle<()>
where
    F: 'static + Send + FnOnce() -> Result<()>,
{
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            if let Err(e) = f() {
                warn!("{} thread failed: {}", name, e);
            }
        })
        .expect("failed to spawn a thread")
}

struct Peer {
    id: usize,
    client: Client,
    stream: TcpStream,
}

impl Peer {
    fn new(id: usize, stream: TcpStream) -> Self {
        Self {
            id,
            client: Client::default(),
            stream,
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn tip_receiver(config: &Config) -> Result<Receiver<BlockHash>> {
    let (tip_tx, tip_rx) = bounded(0);
    let rpc = rpc_connect(&config)?;

    let duration = u64::try_from(config.wait_duration.as_millis()).unwrap();

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
    let tip_rx = tip_receiver(&config)?;
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
        peers = notify_peers(&rpc, peers);
    }
    info!("stopping Electrum RPC server");
    Ok(())
}

fn notify_peers(rpc: &Rpc, peers: HashMap<usize, Peer>) -> HashMap<usize, Peer> {
    // peers are dropped and disconnected on error.
    peers
        .into_par_iter()
        .filter_map(|(peer_id, peer)| match notify_peer(&rpc, peer) {
            Ok(peer) => Some((peer.id, peer)),
            Err(e) => {
                error!("failed to notify peer {}: {}", peer_id, e);
                None
            }
        })
        .collect()
}

fn notify_peer(rpc: &Rpc, mut peer: Peer) -> Result<Peer> {
    // peer is dropped and disconnected on error.
    let notifications = rpc
        .update_client(&mut peer.client)
        .context("failed to generate notifications")?;
    send_to_peer(&mut peer, &notifications).context("failed to send notifications")?;
    Ok(peer)
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
    match event.msg {
        Message::New(stream) => {
            debug!("{}: connected", event.peer_id);
            peers.insert(event.peer_id, Peer::new(event.peer_id, stream));
        }
        Message::Request(line) => {
            let result = match peers.get_mut(&event.peer_id) {
                Some(peer) => handle_request(rpc, peer, line),
                None => {
                    warn!("{}: unknown peer for {}", event.peer_id, line);
                    Ok(())
                }
            };
            if let Err(e) = result {
                error!("{}: disconnecting due to {}", event.peer_id, e);
                drop(peers.remove(&event.peer_id)); // disconnect peer due to error
            }
        }
        Message::Done => {
            debug!("{}: disconnected", event.peer_id);
            peers.remove(&event.peer_id);
        }
    }
}

fn handle_request(rpc: &Rpc, peer: &mut Peer, line: String) -> Result<()> {
    let request: Value = from_str(&line).with_context(|| format!("invalid request: {}", line))?;
    let response: Value = rpc
        .handle_request(&mut peer.client, request)
        .with_context(|| format!("failed to handle request: {}", line))?;
    send_to_peer(peer, &[response])
}

fn send_to_peer(peer: &mut Peer, values: &[Value]) -> Result<()> {
    for value in values {
        let mut response = value.to_string();
        debug!("{}: send {}", peer.id, response);
        response += "\n";
        peer.stream
            .write_all(response.as_bytes())
            .with_context(|| format!("failed to send response: {}", response))?;
    }
    Ok(())
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
    server_tx.send(Event {
        peer_id,
        msg: Message::New(stream.try_clone()?),
    })?;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line.with_context(|| format!("{}: recv failed", peer_id))?;
        debug!("{}: recv {}", peer_id, line);
        let msg = Message::Request(line);
        server_tx.send(Event { peer_id, msg })?;
    }
    server_tx.send(Event {
        peer_id,
        msg: Message::Done,
    })?;
    Ok(())
}
