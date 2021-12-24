use anyhow::{Context, Result};
use crossbeam_channel::{select, unbounded, Sender};
use rayon::prelude::*;

use std::{
    collections::hash_map::HashMap,
    io::{BufRead, BufReader, Write},
    iter::once,
    net::{Shutdown, TcpListener, TcpStream},
};

use crate::{
    config::Config,
    electrum::{Client, Rpc},
    metrics::{self, Metrics},
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
    let metrics = Metrics::new(config.monitoring_addr)?;

    let (server_tx, server_rx) = unbounded();
    if !config.disable_electrum_rpc {
        let listener = TcpListener::bind(config.electrum_rpc_addr)?;
        info!("serving Electrum RPC on {}", listener.local_addr()?);
        spawn("accept_loop", || accept_loop(listener, server_tx)); // detach accepting thread
    };

    let server_batch_size = metrics.histogram_vec(
        "server_batch_size",
        "# of server events handled in a single batch",
        "type",
        metrics::default_size_buckets(),
    );
    let duration = metrics.histogram_vec(
        "server_loop_duration",
        "server loop duration",
        "step",
        metrics::default_duration_buckets(),
    );
    let mut rpc = Rpc::new(&config, metrics)?;

    let new_block_rx = rpc.new_block_notification();
    let mut peers = HashMap::<usize, Peer>::new();
    loop {
        // initial sync and compaction may take a few hours
        while server_rx.is_empty() {
            let done = duration.observe_duration("sync", || rpc.sync().context("sync failed"))?; // sync a batch of blocks
            peers = duration.observe_duration("notify", || notify_peers(&rpc, peers)); // peers are disconnected on error
            if !done {
                continue; // more blocks to sync
            }
            if config.sync_once {
                return Ok(()); // exit after initial sync is done
            }
            break;
        }
        duration.observe_duration("select", || -> Result<()> {
            select! {
                // Handle signals for graceful shutdown
                recv(rpc.signal().receiver()) -> result => {
                    result.context("signal channel disconnected")?;
                    rpc.signal().exit_flag().poll().context("RPC server interrupted")?;
                },
                // Handle new blocks' notifications
                recv(new_block_rx) -> result => match result {
                    Ok(_) => (), // sync and update
                    Err(_) => {
                        info!("disconnected from bitcoind");
                        return Ok(());
                    }
                },
                // Handle Electrum RPC requests
                recv(server_rx) -> event => {
                    let first = once(event.context("server disconnected")?);
                    let rest = server_rx.iter().take(server_rx.len());
                    let events: Vec<Event> = first.chain(rest).collect();
                    server_batch_size.observe("recv", events.len() as f64);
                    duration.observe_duration("handle", || handle_events(&rpc, &mut peers, events));
                },
                default(config.wait_duration) => (), // sync and update
            };
            Ok(())
        })?;
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

fn handle_events(rpc: &Rpc, peers: &mut HashMap<usize, Peer>, events: Vec<Event>) {
    let mut events_by_peer = HashMap::<usize, Vec<Message>>::new();
    events
        .into_iter()
        .for_each(|e| events_by_peer.entry(e.peer_id).or_default().push(e.msg));
    for (peer_id, messages) in events_by_peer {
        handle_peer_events(rpc, peers, peer_id, messages);
    }
}

fn handle_peer_events(
    rpc: &Rpc,
    peers: &mut HashMap<usize, Peer>,
    peer_id: usize,
    messages: Vec<Message>,
) {
    let mut lines = vec![];
    let mut done = false;
    for msg in messages {
        match msg {
            Message::New(stream) => {
                debug!("{}: connected", peer_id);
                peers.insert(peer_id, Peer::new(peer_id, stream));
            }
            Message::Request(line) => lines.push(line),
            Message::Done => {
                done = true;
                break;
            }
        }
    }
    let result = match peers.get_mut(&peer_id) {
        Some(peer) => {
            let responses = rpc.handle_requests(&mut peer.client, &lines);
            peer.send(responses)
        }
        None => return, // unknown peer
    };
    if let Err(e) = result {
        error!("{}: disconnecting due to {}", peer_id, e);
        peers.remove(&peer_id).unwrap().disconnect();
    } else if done {
        peers.remove(&peer_id); // already disconnected, just remove from peers' map
    }
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

    let mut first_line = true;
    for line in BufReader::new(stream).lines() {
        if let Err(e) = &line {
            if first_line && e.kind() == std::io::ErrorKind::InvalidData {
                warn!("InvalidData on first line may indicate client attempted to connect using SSL when server expects unencrypted communication.")
            }
        }
        let line = line.with_context(|| format!("{}: recv failed", peer_id))?;
        debug!("{}: recv {}", peer_id, line);
        let msg = Message::Request(line);
        server_tx.send(Event { peer_id, msg })?;
        first_line = false;
    }

    debug!("{}: disconnected", peer_id);
    let msg = Message::Done;
    server_tx.send(Event { peer_id, msg })?;
    Ok(())
}
