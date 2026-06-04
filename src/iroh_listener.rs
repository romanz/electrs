use anyhow::Result;
use iroh::{Endpoint, SecretKey, endpoint::presets};
use std::io::{Read, Write};
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
        eprintln!("Iroh connection from: {}", conn.remote_id());

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

    let (mut iroh_send, mut iroh_recv) = conn.accept_bi().await?;

    // Socketpair: fd_a → electrs, fd_b → wir
    let mut fds = [0i32; 2];
    let ret = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    if ret != 0 {
        anyhow::bail!("socketpair failed");
    }
    let (fd_electrs, fd_ours) = (fds[0], fds[1]);

    // electrs bekommt fd_electrs als TcpStream
    let tcp_for_electrs = unsafe { TcpStream::from_raw_fd(fd_electrs) };

    // Unsere Seite: lesen und schreiben
    let mut sock_read = unsafe { TcpStream::from_raw_fd(fd_ours) };
    let mut sock_write = sock_read.try_clone()?;

    // electrs benachrichtigen: neue Verbindung
    server_tx.send(Event { peer_id, msg: Message::New(tcp_for_electrs) })?;

    // Thread 1: Iroh → Socket (Wallet sendet Anfrage → electrs)
    let tx2 = server_tx.clone();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 4096];
        loop {
            // Iroh lesen (blockierend via tokio block_in_place nicht möglich hier,
            // daher nutzen wir einen sync-Kanal)
            // Dieser Thread wartet auf Daten vom Socket (von electrs geschrieben)
            // und leitet sie an Iroh weiter
            match sock_read.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    // Das ist der electrs→Wallet Weg (Antworten)
                    // wird im nächsten Schritt verbunden
                    let _ = n;
                }
                Err(_) => break,
            }
        }
    });

    // Iroh recv → server_tx (zeilenweise, wie TCP)
    let mut iroh_buf = Vec::new();

    loop {
        match iroh_recv.read_chunk(4096).await {
            Ok(Some(chunk)) => {
                iroh_buf.extend_from_slice(&chunk);
            }
            Ok(None) | Err(_) => break,
        };

        // Zeilenweise verarbeiten (Electrum-Protokoll ist newline-delimited)
        while let Some(pos) = iroh_buf.iter().position(|&b| b == b'\n') {
            let line = iroh_buf.drain(..=pos).collect::<Vec<_>>();
            let line = String::from_utf8_lossy(&line).trim().to_string();
            if !line.is_empty() {
                server_tx.send(Event { peer_id, msg: Message::Request(line) })?;
            }
        }
    }

    server_tx.send(Event { peer_id, msg: Message::Done })?;
    Ok(())
}
