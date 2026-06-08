use anyhow::Result;
use iroh::{Endpoint, SecretKey};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::io::FromRawFd;
use crossbeam_channel::Sender;

const ELECTRUM_ALPN: &[u8] = b"electrs/electrum/0";

pub fn load_or_generate_secret_key() -> Result<SecretKey> {
    let path = std::path::PathBuf::from("/data/iroh_secret_key.bin");

    if path.exists() {
        let bytes = std::fs::read(&path)?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid key file"))?;
        eprintln!("Iroh: loaded existing secret key from {:?}", path);
        Ok(SecretKey::from_bytes(&arr))
    } else {
        let key = SecretKey::generate(rand::rngs::OsRng);
        std::fs::write(&path, key.to_bytes())?;
        eprintln!("Iroh: new secret key generated and saved to {:?}", path);
        Ok(key)
    }
}

pub async fn run_iroh_listener(server_tx: Sender<crate::server::Event>, secret_key: SecretKey) -> Result<()> {
    let endpoint = Endpoint::builder()
        .secret_key(secret_key)
        .alpns(vec![ELECTRUM_ALPN.to_vec()])
        .bind()
        .await?;

    let node_id = endpoint.node_id();
    eprintln!("Iroh Node ID: {node_id}");
    if let Err(e) = std::fs::write("/data/iroh_node_id.txt", node_id.to_string()) {
        eprintln!("Iroh: could not write node_id to file: {e}");
    }

    let addr = endpoint.node_addr().await?;
    if let Some(relay_url) = addr.relay_url() {
        eprintln!("Iroh Relay URL: {relay_url}");
    }
    eprintln!("Iroh endpoint online");

    let mut peer_counter: usize = 10000;

    while let Some(incoming) = endpoint.accept().await {
        let accepting = match incoming.accept() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("incoming failed: {e}");
                continue;
            }
        };
        let conn = match accepting.await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Iroh: connection handshake failed: {e}");
                continue;
            }
        };
        eprintln!("Iroh connection from: {}", conn.remote_node_id().map(|k| k.to_string()).unwrap_or_default());

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

    let mut fds = [0i32; 2];
    let ret = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    if ret != 0 {
        anyhow::bail!("socketpair failed");
    }
    let (fd_electrs, fd_ours) = (fds[0], fds[1]);

    let tcp_for_electrs = unsafe { TcpStream::from_raw_fd(fd_electrs) };
    let mut sock_read = unsafe { TcpStream::from_raw_fd(fd_ours) };
    let mut sock_write = sock_read.try_clone()?;

    server_tx.send(Event { peer_id, msg: Message::New(tcp_for_electrs) })?;

    let (reply_tx, reply_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 4096];
        loop {
            match sock_read.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if reply_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    tokio::spawn(async move {
        while let Ok(data) = reply_rx.recv() {
            if iroh_send.write_all(&data).await.is_err() {
                break;
            }
        }
        let _ = iroh_send.finish();
    });

    let mut iroh_buf = Vec::new();

    loop {
        let mut tmp = vec![0u8; 4096];
        match iroh_recv.read(&mut tmp).await {
            Ok(Some(n)) => {

                sock_write.write_all(&tmp[..n])?;
                iroh_buf.extend_from_slice(&tmp[..n]);
            }
            Ok(None) => { sock_write.shutdown(std::net::Shutdown::Write).ok(); break; }
            Err(e) => { eprintln!("Iroh peer {peer_id}: read error: {e}"); break; }
        };

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
