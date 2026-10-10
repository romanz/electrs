use anyhow::{Context, Result};
use crossbeam_channel::{select, tick, unbounded, Receiver, Sender};

use std::{
    collections::hash_map::HashMap,
    io::{BufRead, BufReader, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{Arc, RwLock, TryLockError},
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
    let rpc = Rpc::new(&config, metrics)?;
    let signals = rpc.signal().receiver().clone();
    let exit_flag = rpc.signal().exit_flag().clone();
    let rpc = Arc::new(RwLock::new(rpc));
    let sync_timer = tick(config.wait_duration);
    let mut sync_pending = true;
    let mut peers = HashMap::<usize, Sender<PeerMessage>>::new();
    loop {
        if sync_pending {
            // Never queue a writer behind a slow wallet: waiting writers can block
            // new readers, preventing other clients from making requests.
            match rpc.try_write() {
                Ok(mut rpc) => {
                    let done =
                        duration.observe_duration("sync", || rpc.sync().context("sync failed"))?;
                    sync_pending = !done;
                    if done {
                        peers.retain(|_, tx| tx.send(PeerMessage::Notify).is_ok());
                        if config.sync_once {
                            return Ok(());
                        }
                    } else if server_rx.is_empty() {
                        continue; // keep indexing without waiting for the next timer tick
                    }
                }
                Err(TryLockError::WouldBlock) => (),
                Err(TryLockError::Poisoned(_)) => bail!("RPC lock poisoned"),
            }
        }
        select! {
            recv(signals) -> result => {
                result.context("signal channel disconnected")?;
                exit_flag.poll().context("RPC server interrupted")?;
                sync_pending = true;
            },
            recv(server_rx) -> event => {
                let event = event.context("server disconnected")?;
                server_batch_size.observe("recv", 1.0);
                match event.msg {
                    Message::New(stream) => {
                        let rpc = Arc::clone(&rpc);
                        let duration = duration.clone();
                        let tx = spawn_peer(event.peer_id, stream, move |client, message| {
                            let rpc = rpc.read().map_err(|_| anyhow!("RPC lock poisoned"))?;
                            match message {
                                PeerMessage::Request(line) => duration.observe_duration("handle", || {
                                    Ok(rpc.handle_requests(client, &[line]))
                                }),
                                PeerMessage::Notify => duration.observe_duration("notify", || {
                                    rpc.update_client(client)
                                }),
                            }
                        });
                        peers.insert(event.peer_id, tx);
                    }
                    Message::Request(line) => {
                        if let Some(tx) = peers.get(&event.peer_id) {
                            if tx.send(PeerMessage::Request(line)).is_err() {
                                peers.remove(&event.peer_id);
                            }
                        }
                    }
                    Message::Done => {
                        peers.remove(&event.peer_id);
                    }
                }
            },
            recv(sync_timer) -> _ => sync_pending = true,
        }
    }
}

enum PeerMessage {
    Request(String),
    Notify,
}

fn spawn_peer<F>(peer_id: usize, stream: TcpStream, mut handle: F) -> Sender<PeerMessage>
where
    F: FnMut(&mut Client, PeerMessage) -> Result<Vec<String>> + Send + 'static,
{
    let (tx, rx) = unbounded();
    spawn("peer_loop", move || {
        debug!("{}: connected", peer_id);
        let mut peer = Peer::new(peer_id, stream);
        let result = serve_peer(&mut peer, rx, &mut handle);
        // Close both directions even on a read/handler/write error, so the
        // receiving thread and any queued requests cannot outlive the peer.
        peer.disconnect();
        result
    });
    tx
}

fn serve_peer(
    peer: &mut Peer,
    rx: Receiver<PeerMessage>,
    mut handle: impl FnMut(&mut Client, PeerMessage) -> Result<Vec<String>>,
) -> Result<()> {
    for message in rx {
        let responses = handle(&mut peer.client, message)?;
        // The RPC read lock has been released before a potentially slow write.
        peer.send(responses)?;
    }
    Ok(())
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

fn accept_loop(listener: TcpListener, server_tx: Sender<Event>) -> Result<()> {
    for (peer_id, conn) in listener.incoming().enumerate() {
        let stream = conn.context("failed to accept")?;
        let tx = server_tx.clone();
        spawn("recv_loop", move || {
            let result = recv_loop(peer_id, &stream, tx.clone());
            let _ = tx.send(Event {
                peer_id,
                msg: Message::Done,
            });
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::BufRead, time::Duration};

    fn connection() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        (client, listener.accept().unwrap().0)
    }

    #[test]
    fn slow_peer_does_not_block_another_peer() {
        let rpc = Arc::new(RwLock::new(()));
        let (started_tx, started_rx) = unbounded();
        let (release_tx, release_rx) = unbounded();
        let (mut slow_client, slow_stream) = connection();
        let slow_rpc = Arc::clone(&rpc);
        let slow = spawn_peer(0, slow_stream, move |_, message| {
            if matches!(message, PeerMessage::Request(ref line) if line == "queued scan") {
                return Ok(vec!["queued".into()]);
            }
            let _read = slow_rpc.read().unwrap();
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(vec!["slow".into()])
        });
        slow.send(PeerMessage::Request("scan".into())).unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(rpc.try_write(), Err(TryLockError::WouldBlock)));

        let (fast_client, fast_stream) = connection();
        let fast = spawn_peer(1, fast_stream, move |_, _| {
            let _read = rpc.read().unwrap();
            Ok(vec!["pong".into()])
        });
        fast.send(PeerMessage::Request("ping".into())).unwrap();
        let mut response = String::new();
        BufReader::new(fast_client)
            .read_line(&mut response)
            .unwrap();
        assert_eq!(response, "pong\n"); // slow request is still waiting

        // A read EOF must still allow queued requests to receive responses.
        slow.send(PeerMessage::Request("queued scan".into()))
            .unwrap();
        drop(slow);
        release_tx.send(()).unwrap();
        response.clear();
        let mut reader = BufReader::new(&mut slow_client);
        reader.read_line(&mut response).unwrap();
        assert_eq!(response, "slow\n");
        response.clear();
        reader.read_line(&mut response).unwrap();
        assert_eq!(response, "queued\n");
        assert_eq!(reader.read_line(&mut String::new()).unwrap(), 0);
    }

    #[test]
    fn peer_preserves_request_and_notification_order() {
        let (client, stream) = connection();
        let worker = spawn_peer(0, stream, |_, message| {
            Ok(vec![match message {
                PeerMessage::Request(line) => line,
                PeerMessage::Notify => "notification".into(),
            }])
        });
        worker.send(PeerMessage::Request("first".into())).unwrap();
        worker.send(PeerMessage::Notify).unwrap();
        worker.send(PeerMessage::Request("second".into())).unwrap();
        let mut reader = BufReader::new(client);
        for expected in ["first\n", "notification\n", "second\n"] {
            let mut response = String::new();
            reader.read_line(&mut response).unwrap();
            assert_eq!(response, expected);
        }
    }

    #[test]
    fn handler_error_closes_the_connection() {
        let (client, stream) = connection();
        let worker = spawn_peer(0, stream, |_, _| bail!("failed request"));
        worker.send(PeerMessage::Request("fail".into())).unwrap();
        assert_eq!(
            BufReader::new(client)
                .read_line(&mut String::new())
                .unwrap(),
            0
        );
    }
}
