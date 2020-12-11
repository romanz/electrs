#![recursion_limit = "256"]

mod mempool;
mod rpc;
mod util;

#[macro_use]
extern crate log;

use anyhow::{Context, Result};
use async_signals::Signals;
use async_std::{
    future,
    io::BufReader,
    net::{TcpListener, TcpStream, ToSocketAddrs},
    prelude::*,
    task,
};

use futures::{
    sink::SinkExt,
    stream::StreamExt,
    {select, FutureExt},
};
use serde_json::{de::from_str, Value};

use std::{
    collections::hash_map::{Entry, HashMap},
    path::Path,
    sync::Arc,
    time::Duration,
};

use electrs_index::*;
use rpc::{Indexer, Rpc, Subscription};
use util::{spawn, unbounded, Receiver, Sender};

fn main() -> Result<()> {
    let config = Config::from_args();
    let metrics = Metrics::new(config.monitoring_addr)?;
    let daemon = Daemon::new(config.daemon_rpc_addr, &config.daemon_dir)
        .context("failed to connect to daemon")?;
    let store = DBStore::open(Path::new(&config.db_path), config.low_memory)?;
    let index = Index::new(store, &metrics, config.low_memory).context("failed to open index")?;
    let rpc = Rpc::new(index, daemon, &metrics);

    let handle = task::spawn(accept_loop(config.electrum_rpc_addr, rpc));
    task::block_on(handle)
}

#[derive(Debug)]
enum Void {}

#[derive(Debug)]
enum Event {
    NewPeer {
        id: usize,
        stream: Arc<TcpStream>,
        shutdown: Receiver<Void>,
    },
    Message {
        id: usize,
        req: Value,
    },
}

async fn accept_loop(addr: impl ToSocketAddrs, rpc: Rpc) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let mut incoming = listener.incoming();
    let mut count = 0usize;
    info!("serving Electrum RPC on {}", listener.local_addr()?);

    let (broker_tx, broker_rx) = unbounded();
    let mut broker_shutdown = task::spawn(server_loop(broker_rx, rpc)).into_stream();

    loop {
        select! {
            result = incoming.next().fuse() => {
                let stream = result.expect("missing stream")?;
                spawn("recv_loop", recv_loop(count, broker_tx.clone(), stream));
                count += 1;
            },
            result = broker_shutdown.next().fuse() => {
                debug!("accept_loop is done");
                return result.expect("missing result"); // TODO: takes ~0.5s?
            },
        };
    }
}

struct Peer {
    subscription: Subscription,
    sender: Sender<Value>,
}

impl Peer {
    fn new(sender: Sender<Value>) -> Self {
        Self {
            subscription: Subscription::new(),
            sender,
        }
    }
}

async fn server_loop(events: Receiver<Event>, rpc: Rpc) -> Result<()> {
    let mut peers: HashMap<usize, Peer> = HashMap::new();

    let (disconnect_tx, disconnect_rx) = unbounded::<(usize, Receiver<Value>)>();
    let mut disconnect_rx = disconnect_rx.fuse();
    let mut events = events.fuse();
    let mut signals = Signals::new(vec![libc::SIGINT, libc::SIGUSR1])
        .context("failed to register signal handler")?
        .fuse();
    let mut new_block_rx = rpc.start_waiter()?;

    let Indexer {
        result: index_result,
        notify: mut index_notify,
        task: index_task,
    } = rpc.start_indexer()?;
    let mut index_result = index_result.fuse();

    loop {
        notify_peers(&rpc, &mut peers).await?;
        let event = select! {
            sig = future::timeout(Duration::from_secs(5), signals.next()).fuse() => {
                match sig {
                    Ok(Some(libc::SIGUSR1)) | Err(_) => {
                        rpc.sync_mempool();
                        continue;
                    },
                    Ok(Some(s)) => {
                        debug!("got SIG{}, stopping indexer", s);
                        rpc.stop_indexer();
                        break;
                    },
                    Ok(None) => panic!("missing signal"),
                };
            }
            msg = new_block_rx.next() => {
                match msg {
                    Some(tip) => {
                        debug!("notifying indexer: {:?}", tip);
                        index_notify.send(()).await.expect("failed to notify indexer");
                        continue;
                    },
                    None => break,  // waiter has exited
                }
            },
            msg = index_result.next() => {
                match msg {
                    Some(result) => {
                        info!("indexing finished: {:?}", result);
                        // TODO: rate-limit?
                        rpc.sync_mempool();  // remove confirmed transactions from mempool
                        continue;
                    },
                    None => break,  // indexer has exited
                }
            },
            disconnect = disconnect_rx.next() => {
                let (id, _pending_messages) = disconnect.expect("missing disconnected ID");
                info!("{}: disconnected", id);
                assert!(peers.remove(&id).is_some());
                continue;
            },
            event = events.next() => match event {
                Some(event) => event,
                None => break,
            },
        };
        handle_event(&rpc, &mut peers, event, &disconnect_tx).await?;
    }
    drop(index_notify);
    debug!("waiting for index_task");
    task::block_on(index_task).context("index_task failed")?;
    while let Some(_) = index_result.next().await {}
    debug!("disconnecting {} clients: {:?}", peers.len(), peers.keys());
    drop(peers); // drop all senders that write responses
    drop(disconnect_tx);
    while let Some((id, _sender_rx)) = disconnect_rx.next().await {
        debug!("{}: gone", id)
    }
    debug!("server_loop is done");
    Ok(())
}

async fn notify_peers(rpc: &Rpc, peers: &mut HashMap<usize, Peer>) -> Result<()> {
    for peer in peers.values_mut() {
        let notifications = rpc
            .notify(&mut peer.subscription)
            .context("subscription notification failed")?;
        for notification in notifications {
            peer.sender.send(notification).await.unwrap();
        }
    }
    Ok(())
}

async fn handle_event(
    rpc: &Rpc,
    peers: &mut HashMap<usize, Peer>,
    event: Event,
    disconnect_tx: &Sender<(usize, Receiver<Value>)>,
) -> Result<()> {
    match event {
        Event::Message { id, req } => match peers.get_mut(&id) {
            Some(peer) => {
                let response = rpc
                    .handle_request(&mut peer.subscription, req)
                    .context("RPC failed")?;
                peer.sender.send(response).await.unwrap();
            }
            None => warn!("unknown client {}", id),
        },
        Event::NewPeer {
            id,
            stream,
            shutdown,
        } => match peers.entry(id) {
            Entry::Occupied(..) => panic!("duplicate connection ID: {}", id),
            Entry::Vacant(entry) => {
                let (sender_tx, mut sender_rx) = unbounded();
                entry.insert(Peer::new(sender_tx));
                let mut disconnect_tx = disconnect_tx.clone();
                spawn("send_loop", async move {
                    let res = send_loop(id, &mut sender_rx, stream, shutdown).await;
                    disconnect_tx
                        .send((id, sender_rx))
                        .await
                        .with_context(|| format!("failed to disconnect {}", id))?;
                    res
                });
            }
        },
    }
    Ok(())
}

async fn recv_loop(id: usize, mut broker: Sender<Event>, stream: TcpStream) -> Result<()> {
    info!("{}: accepted {}", id, stream.peer_addr()?);
    let stream = Arc::new(stream);
    let reader = BufReader::new(&*stream);
    let mut lines = reader.lines();
    let (shutdown_tx, shutdown_rx) = unbounded::<Void>();
    broker
        .send(Event::NewPeer {
            id,
            stream: Arc::clone(&stream),
            shutdown: shutdown_rx,
        })
        .await
        .unwrap();

    while let Some(line) = lines.next().await {
        let line = line?;
        debug!("{}: recv {}", id, line);
        let req: Value = from_str(&line).with_context(|| format!("invalid JSON: {:?}", line))?;
        broker
            .send(Event::Message { id, req })
            .await
            .with_context(|| format!("failed to send {:?}", id))?;
    }
    drop(shutdown_tx);
    Ok(())
}

async fn send_loop(
    id: usize,
    messages: &mut Receiver<Value>,
    stream: Arc<TcpStream>,
    shutdown: Receiver<Void>,
) -> Result<()> {
    let mut stream = &*stream;
    let mut messages = messages.fuse();
    let mut shutdown = shutdown.fuse();
    loop {
        select! {
            msg = messages.next().fuse() => match msg {
                Some(msg) => {
                    let line = msg.to_string();
                    debug!("{}: send {}", id, line);
                    stream.write_all((line + "\n").as_bytes()).await?;
                },
                None => {
                    debug!("{}: closed", id);
                    break;
                }
            },
            void = shutdown.next().fuse() => {
                debug!("stopping send_loop");
                match void {
                    Some(void) => match void {},
                    None => break,
                }
            }
        }
    }
    Ok(())
}
