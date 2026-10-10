use anyhow::{Context, Result};
use crossbeam_channel::{bounded, select, tick, Receiver, Sender, TrySendError};

use std::{
    collections::hash_map::HashMap,
    io::{BufRead, BufReader, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{Arc, Condvar, Mutex, RwLock},
    time::Duration,
};

use crate::{
    config::Config,
    electrum::{Client, Rpc},
    metrics::{self, Metrics},
    signals::ExitError,
    thread::spawn,
};

// Bound the extra per-peer workers and apply TCP backpressure to busy peers.
const MAX_PEERS: usize = 128;
const PEER_QUEUE_CAPACITY: usize = 64;
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

struct Admission {
    active: usize,
    sync_pending: bool,
}

// Once sync is requested, existing readers drain before new readers enter.
// The dispatcher never waits for readers and remains able to handle signals.
struct SharedRpc<T> {
    rpc: RwLock<T>,
    admission: Mutex<Admission>,
    available: Condvar,
    max_active: usize,
    ready: Sender<()>,
}

impl<T> SharedRpc<T> {
    fn new(rpc: T, max_active: usize, ready: Sender<()>) -> Self {
        assert!(max_active > 0);
        Self {
            rpc: RwLock::new(rpc),
            admission: Mutex::new(Admission {
                active: 0,
                sync_pending: false,
            }),
            available: Condvar::new(),
            max_active,
            ready,
        }
    }

    fn with_read<R>(&self, f: impl FnOnce(&T) -> R) -> Result<R> {
        let admission = self
            .admission
            .lock()
            .map_err(|_| anyhow!("RPC admission poisoned"))?;
        let mut admission = self
            .available
            .wait_while(admission, |state| {
                state.sync_pending || state.active >= self.max_active
            })
            .map_err(|_| anyhow!("RPC admission poisoned"))?;
        admission.active += 1;
        drop(admission);
        // Drop the RPC read guard before releasing admission, including on panic.
        let _active = ActiveRequest(self);
        let rpc = self.rpc.read().map_err(|_| anyhow!("RPC lock poisoned"))?;
        Ok(f(&rpc))
    }

    fn try_sync<R>(&self, f: impl FnOnce(&mut T) -> Result<R>) -> Result<Option<R>> {
        let mut admission = self
            .admission
            .lock()
            .map_err(|_| anyhow!("RPC admission poisoned"))?;
        admission.sync_pending = true;
        if admission.active != 0 {
            return Ok(None);
        }
        drop(admission);
        let result = {
            let mut rpc = self.rpc.write().map_err(|_| anyhow!("RPC lock poisoned"))?;
            f(&mut rpc)
        };
        self.admission
            .lock()
            .map_err(|_| anyhow!("RPC admission poisoned"))?
            .sync_pending = false;
        self.available.notify_all();
        result.map(Some)
    }
}

struct ActiveRequest<'a, T>(&'a SharedRpc<T>);

impl<T> Drop for ActiveRequest<'_, T> {
    fn drop(&mut self) {
        let mut admission = self.0.admission.lock().expect("RPC admission poisoned");
        admission.active -= 1;
        if admission.active == 0 && admission.sync_pending {
            let _ = self.0.ready.try_send(());
        }
        self.0.available.notify_all();
    }
}

// A slot remains occupied until both receiving and processing have finished.
struct ConnectionSlot(Sender<()>);

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

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
}

impl Drop for Peer {
    fn drop(&mut self) {
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

    let (server_tx, server_rx) = bounded(2 * MAX_PEERS);
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
    let (ready_tx, ready_rx) = bounded(1);
    let rpc = Arc::new(SharedRpc::new(
        rpc,
        rayon::current_num_threads().clamp(2, 8),
        ready_tx,
    ));
    let sync_timer = tick(config.wait_duration);
    let mut sync_pending = true;
    let mut peers = HashMap::<usize, Sender<PeerMessage>>::new();
    loop {
        if sync_pending {
            if let Some(done) = rpc.try_sync(|rpc| {
                duration.observe_duration("sync", || rpc.sync().context("sync failed"))
            })? {
                sync_pending = !done;
                if done {
                    peers.retain(|_, tx| match tx.try_send(PeerMessage::Notify) {
                        Ok(()) | Err(TrySendError::Full(_)) => true,
                        Err(TrySendError::Disconnected(_)) => false,
                    });
                    if config.sync_once {
                        return Ok(());
                    }
                } else if server_rx.is_empty() {
                    continue; // keep indexing without waiting for the next timer tick
                }
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
                    Message::New(stream, tx, rx, slot) => {
                        let rpc = Arc::clone(&rpc);
                        let duration = duration.clone();
                        match spawn_peer(event.peer_id, stream, rx, move |client, message| {
                            let _slot = &slot; // keep the connection slot until this worker exits
                            rpc.with_read(|rpc| match message {
                                PeerMessage::Request(line) => duration.observe_duration("handle", || {
                                    Ok(rpc.handle_requests(client, &[line]))
                                }),
                                PeerMessage::Notify => duration.observe_duration("notify", || {
                                    rpc.update_client(client)
                                }),
                            })?
                        }) {
                            Ok(()) => { peers.insert(event.peer_id, tx); }
                            Err(e) => warn!("{}: failed to start peer: {}", event.peer_id, e),
                        }
                    }
                    Message::Done => {
                        peers.remove(&event.peer_id);
                    }
                }
            },
            recv(sync_timer) -> _ => sync_pending = true,
            recv(ready_rx) -> _ => (),
        }
    }
}

enum PeerMessage {
    Request(String),
    Notify,
}

fn spawn_peer<F>(
    peer_id: usize,
    stream: TcpStream,
    rx: Receiver<PeerMessage>,
    mut handle: F,
) -> Result<()>
where
    F: FnMut(&mut Client, PeerMessage) -> Result<Vec<String>> + Send + 'static,
{
    let mut peer = Peer::new(peer_id, stream);
    peer.stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    std::thread::Builder::new()
        .name("peer_loop".into())
        .spawn(move || {
            debug!("{}: connected", peer_id);
            if let Err(e) = serve_peer(&mut peer, rx, &mut handle) {
                warn!("{}: peer failed: {:#}", peer_id, e);
            }
            // Peer::drop shuts down both directions on errors and during unwinding.
        })
        .context("failed to spawn peer worker")?;
    Ok(())
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
    New(
        TcpStream,
        Sender<PeerMessage>,
        Receiver<PeerMessage>,
        Arc<ConnectionSlot>,
    ),
    Done,
}

fn accept_loop(listener: TcpListener, server_tx: Sender<Event>) -> Result<()> {
    let (slots_tx, slots_rx) = bounded(MAX_PEERS);
    for _ in 0..MAX_PEERS {
        slots_tx.send(())?;
    }
    for (peer_id, conn) in listener.incoming().enumerate() {
        let stream = conn.context("failed to accept")?;
        if slots_rx.try_recv().is_err() {
            warn!("{}: connection limit reached", peer_id);
            let _ = stream.shutdown(Shutdown::Both);
            continue;
        }
        let slot = Arc::new(ConnectionSlot(slots_tx.clone()));
        let tx = server_tx.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("recv_loop".into())
            .spawn(move || {
                let result = recv_loop(peer_id, &stream, tx.clone(), Arc::clone(&slot));
                let _ = tx.send(Event {
                    peer_id,
                    msg: Message::Done,
                });
                if let Err(e) = stream.shutdown(Shutdown::Read) {
                    warn!("{}: failed to shutdown TCP receiving {}", peer_id, e)
                }
                if let Err(e) = result {
                    warn!("{}: receive failed: {:#}", peer_id, e);
                }
            })
        {
            warn!("{}: failed to start receiver: {}", peer_id, e);
        }
    }
    Ok(())
}

fn recv_loop(
    peer_id: usize,
    stream: &TcpStream,
    server_tx: Sender<Event>,
    slot: Arc<ConnectionSlot>,
) -> Result<()> {
    let (tx, rx) = bounded(PEER_QUEUE_CAPACITY);
    let msg = Message::New(stream.try_clone()?, tx.clone(), rx, slot);
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
        tx.send(PeerMessage::Request(line))?;
        first_line = false;
    }

    debug!("{}: disconnected", peer_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::{io::BufRead, time::Duration};

    fn worker<F>(peer_id: usize, stream: TcpStream, handle: F) -> Sender<PeerMessage>
    where
        F: FnMut(&mut Client, PeerMessage) -> Result<Vec<String>> + Send + 'static,
    {
        let (tx, rx) = bounded(PEER_QUEUE_CAPACITY);
        spawn_peer(peer_id, stream, rx, handle).unwrap();
        tx
    }

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
        let (ready_tx, _) = bounded(1);
        let rpc = Arc::new(SharedRpc::new((), 2, ready_tx));
        let (started_tx, started_rx) = unbounded();
        let (release_tx, release_rx) = unbounded();
        let (mut slow_client, slow_stream) = connection();
        let slow_rpc = Arc::clone(&rpc);
        let slow = worker(0, slow_stream, move |_, message| {
            if matches!(message, PeerMessage::Request(ref line) if line == "queued scan") {
                return Ok(vec!["queued".into()]);
            }
            slow_rpc.with_read(|_| {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                vec!["slow".into()]
            })
        });
        slow.send(PeerMessage::Request("scan".into())).unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let (fast_client, fast_stream) = connection();
        let fast = worker(1, fast_stream, move |_, _| {
            rpc.with_read(|_| vec!["pong".into()])
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
        let worker = worker(0, stream, |_, message| {
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
        let worker = worker(0, stream, |_, _| bail!("failed request"));
        worker.send(PeerMessage::Request("fail".into())).unwrap();
        assert_eq!(
            BufReader::new(client)
                .read_line(&mut String::new())
                .unwrap(),
            0
        );
    }

    #[test]
    fn synchronization_precedes_waiting_requests() {
        let (ready_tx, ready_rx) = bounded(1);
        let rpc = Arc::new(SharedRpc::new(0, 2, ready_tx));
        let (started_tx, started_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        let reader_rpc = Arc::clone(&rpc);
        let reader = std::thread::spawn(move || {
            reader_rpc
                .with_read(|_| {
                    started_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                })
                .unwrap()
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(rpc
            .try_sync(|_| -> Result<()> { panic!("reader still active") })
            .unwrap()
            .is_none());

        let (value_tx, value_rx) = bounded(1);
        let waiting_rpc = Arc::clone(&rpc);
        let waiting = std::thread::spawn(move || {
            waiting_rpc
                .with_read(|value| {
                    value_tx.send(*value).unwrap();
                })
                .unwrap()
        });
        // New readers must wait even though the second reader slot is free.
        assert!(value_rx.recv_timeout(Duration::from_millis(50)).is_err());
        release_tx.send(()).unwrap();
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            rpc.try_sync(|value| {
                *value = 1;
                Ok(())
            })
            .unwrap(),
            Some(())
        );
        assert_eq!(value_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        reader.join().unwrap();
        waiting.join().unwrap();
    }

    #[test]
    fn active_requests_are_bounded() {
        let (ready_tx, _) = bounded(1);
        let rpc = Arc::new(SharedRpc::new((), 1, ready_tx));
        let (started_tx, started_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        let reader_rpc = Arc::clone(&rpc);
        let reader = std::thread::spawn(move || {
            reader_rpc
                .with_read(|_| {
                    started_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                })
                .unwrap()
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (entered_tx, entered_rx) = bounded(1);
        let waiting_rpc = Arc::clone(&rpc);
        let waiting = std::thread::spawn(move || {
            waiting_rpc
                .with_read(|_| {
                    entered_tx.send(()).unwrap();
                })
                .unwrap()
        });
        assert!(entered_rx.recv_timeout(Duration::from_millis(50)).is_err());
        release_tx.send(()).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        reader.join().unwrap();
        waiting.join().unwrap();
    }

    #[test]
    fn full_peer_queue_backpressures_only_that_receiver() {
        let (client, stream) = connection();
        let (started_tx, started_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        let worker = worker(0, stream, move |_, _| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            bail!("close busy peer")
        });
        worker.send(PeerMessage::Request("first".into())).unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..PEER_QUEUE_CAPACITY {
            worker
                .try_send(PeerMessage::Request("queued".into()))
                .unwrap();
        }
        assert!(matches!(
            worker.try_send(PeerMessage::Notify),
            Err(TrySendError::Full(_))
        ));
        release_tx.send(()).unwrap();
        assert_eq!(
            BufReader::new(client)
                .read_line(&mut String::new())
                .unwrap(),
            0
        );
        assert!(worker
            .send(PeerMessage::Request("after close".into()))
            .is_err());
    }

    #[test]
    fn handler_panic_releases_admission_and_closes_both_socket_handles() {
        let (ready_tx, _) = bounded(1);
        let rpc = SharedRpc::new((), 1, ready_tx);
        let (client, stream) = connection();
        let receiver_stream = stream.try_clone().unwrap();
        let (tx, rx) = bounded(1);
        tx.send(PeerMessage::Notify).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut peer = Peer::new(0, stream);
            let _ = serve_peer(&mut peer, rx, |_, _| {
                rpc.with_read(|_| -> Result<Vec<String>> { panic!("handler panicked") })?
            });
        }));
        assert!(result.is_err());
        assert_eq!(
            BufReader::new(client)
                .read_line(&mut String::new())
                .unwrap(),
            0
        );
        assert_eq!(
            BufReader::new(receiver_stream)
                .read_line(&mut String::new())
                .unwrap(),
            0
        );
        assert_eq!(rpc.try_sync(|_| Ok(())).unwrap(), Some(()));
    }

    #[test]
    fn blocked_socket_write_does_not_hold_rpc_admission() {
        let (ready_tx, _) = bounded(1);
        let rpc = Arc::new(SharedRpc::new((), 1, ready_tx));
        let (slow_client, stream) = connection();
        let (handled_tx, handled_rx) = bounded(1);
        let writing_rpc = Arc::clone(&rpc);
        let worker = worker(0, stream, move |_, _| {
            let response = writing_rpc.with_read(|_| vec!["x".repeat(16 * 1024 * 1024)])?;
            handled_tx.send(()).unwrap();
            Ok(response)
        });
        worker
            .send(PeerMessage::Request("large response".into()))
            .unwrap();
        handled_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // The response cannot fit in the socket buffers, and the client does not
        // read it. Synchronization must still acquire exclusive access immediately.
        assert_eq!(rpc.try_sync(|_| Ok(())).unwrap(), Some(()));
        slow_client.shutdown(Shutdown::Both).unwrap();
        drop(worker);
    }

    #[test]
    fn connection_slot_is_reused_only_after_both_owners_finish() {
        let (tx, rx) = bounded(1);
        let receiving = Arc::new(ConnectionSlot(tx));
        let processing = Arc::clone(&receiving);
        drop(receiving);
        assert!(rx.try_recv().is_err());
        drop(processing);
        assert_eq!(rx.try_recv(), Ok(()));
    }
}
