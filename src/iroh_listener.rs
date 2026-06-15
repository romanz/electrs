use anyhow::Result;
use iroh::{Endpoint, SecretKey, endpoint::presets};
use std::net::TcpStream;
use std::os::unix::io::FromRawFd;
use crossbeam_channel::Sender;

const ELECTRUM_ALPN: &[u8] = b"electrs/electrum/0";

pub fn load_or_generate_secret_key() -> Result<SecretKey> {
    let path = std::path::PathBuf::from("/data/iroh_secret_key.bin");
    if path.exists() {
        let bytes = std::fs::read(&path)?;
        match bytes.try_into() {
            Ok(arr) => {
                let arr: [u8; 32] = arr;
                eprintln!("Iroh: loaded existing secret key");
                return Ok(SecretKey::from_bytes(&arr));
            }
            Err(_) => {
                eprintln!("Iroh: invalid key file, regenerating");
                std::fs::remove_file(&path).ok();
            }
        }
    }
    let key = SecretKey::generate();
    std::fs::write(&path, key.to_bytes())?;
    eprintln!("Iroh: new secret key generated");
    Ok(key)
}

pub async fn run_iroh_listener(server_tx: Sender<crate::server::Event>, secret_key: SecretKey) -> Result<()> {
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .alpns(vec![ELECTRUM_ALPN.to_vec()])
        .bind()
        .await?;

    let endpoint_id = endpoint.id();
    eprintln!("Iroh Endpoint ID: {endpoint_id}");
    std::fs::write("/data/iroh_node_id.txt", endpoint_id.to_string()).ok();

    for relay_url in endpoint.addr().relay_urls() {
        eprintln!("Iroh Relay: {relay_url}");
    }
    eprintln!("Iroh endpoint online, waiting for connections...");

    let mut peer_counter: usize = 10000;

    while let Some(incoming) = endpoint.accept().await {
        let accepting = match incoming.accept() {
            Ok(a) => a,
            Err(e) => { eprintln!("accept error: {e}"); continue; }
        };
        let conn = match accepting.await {
            Ok(c) => c,
            Err(e) => { eprintln!("handshake failed: {e}"); continue; }
        };
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
    let (mut iroh_send, mut iroh_recv) = conn.accept_bi().await?;

    // Socketpair: fd_electrs geht an recv_loop, fd_ours bridgen wir zu Iroh
    let mut fds = [0i32; 2];
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) } != 0 {
        anyhow::bail!("socketpair failed");
    }
    let (fd_electrs, fd_ours) = (fds[0], fds[1]);
    let tcp_for_electrs = unsafe { TcpStream::from_raw_fd(fd_electrs) };
    let sock_ours_write = unsafe { TcpStream::from_raw_fd(fd_ours) };
    let mut sock_ours_read = sock_ours_write.try_clone()?;

    // recv_loop übernimmt: sendet New, Request, Done Events an electrs
    std::thread::spawn(move || {
        if let Err(e) = crate::server::recv_loop(peer_id, &tcp_for_electrs, server_tx) {
            eprintln!("Iroh peer {peer_id}: recv_loop: {e}");
        }
    });

    // Iroh → sock_ours (Daten von Bridge zu electrs)
    let mut sock_write = sock_ours_write;
    tokio::spawn(async move {
        loop {
            let mut buf = vec![0u8; 4096];
            match iroh_recv.read(&mut buf).await {
                Ok(Some(n)) => {
                    if std::io::Write::write_all(&mut sock_write, &buf[..n]).is_err() { break; }
                }
                _ => { sock_write.shutdown(std::net::Shutdown::Write).ok(); break; }
            }
        }
    });

    // sock_ours → Iroh (Antworten von electrs zurück zur Bridge)
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = vec![0u8; 4096];
        loop {
            match sock_ours_read.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => { if tx.send(buf[..n].to_vec()).is_err() { break; } }
            }
        }
    });

    while let Ok(data) = rx.recv() {
        iroh_send.write_all(&data).await?;
    }
    let _ = iroh_send.finish();

    Ok(())
}
