use anyhow::Result;
use iroh::{Endpoint, SecretKey, endpoint::presets};
use std::net::TcpStream;
use std::os::unix::io::FromRawFd;
use crossbeam_channel::Sender;

const ELECTRUM_ALPN: &[u8] = b"electrs/electrum/0";

pub async fn run_iroh_listener(server_tx: Sender<crate::server::Event>) -> Result<()> {
    let secret_key = SecretKey::generate();

    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .alpns(vec![ELECTRUM_ALPN.to_vec()])
        .bind()
        .await?;

    let node_id = endpoint.id();
    eprintln!("Iroh Node ID: {node_id}");
    endpoint.online().await;
    eprintln!("Iroh endpoint online");

    let mut peer_counter: usize = 10000;

    while let Some(incoming) = endpoint.accept().await {
        let mut accepting = match incoming.accept() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("incoming failed: {e}");
                continue;
            }
        };
        let conn = accepting.await?;
        let remote_id = conn.remote_id();
        eprintln!("Iroh connection from: {remote_id}");

        let peer_id = peer_counter;
        peer_counter += 1;
        let tx = server_tx.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_iroh_conn(peer_id, conn, tx).await {
                eprintln!("Iroh peer {peer_id} error: {e}");
            }
        });
    }
    Ok(())
}

async fn handle_iroh_conn(
    peer_id: usize,
    conn: iroh::endpoint::Connection,
    server_tx: Sender<crate::server::Event>,
) -> Result<()> {
    use crate::server::{Event, Message};

    let (mut send, mut recv) = conn.accept_bi().await?;

    // Unix socketpair: zwei verbundene Sockets
    let (fd_a, fd_b) = {
        let mut fds = [0i32; 2];
        unsafe {
            libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr());
        }
        (fds[0], fds[1])
    };

    // electrs bekommt fd_a als TcpStream
    let tcp_for_electrs = unsafe { TcpStream::from_raw_fd(fd_a) };
    let tcp_clone = tcp_for_electrs.try_clone()?;

    // fd_b: Iroh schreibt rein / liest raus
    let write_sock = unsafe { std::net::TcpStream::from_raw_fd(fd_b) };
    let read_sock = write_sock.try_clone()?;

    // electrs benachrichtigen: neue Verbindung
    server_tx.send(Event { peer_id, msg: Message::New(tcp_for_electrs) })?;

    // Iroh → electrs (empfangen von Wallet, weiterleiten an electrs)
    let tx2 = server_tx.clone();
    tokio::spawn(async move {
        loop {
            match recv.read_to_end(1024 * 64).await {
                Ok(data) => {
                    let line = String::from_utf8_lossy(&data).to_string();
                    if tx2.send(Event { peer_id, msg: Message::Request(line) }).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx2.send(Event { peer_id, msg: Message::Done });
    });

    Ok(())
}
