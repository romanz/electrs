use anyhow::Result;
use iroh::{Endpoint, SecretKey, endpoint::presets};

const ELECTRUM_ALPN: &[u8] = b"electrs/electrum/0";

pub async fn run_iroh_listener() -> Result<()> {
    let secret_key = SecretKey::generate();

    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .alpns(vec![ELECTRUM_ALPN.to_vec()])
        .bind()
        .await?;

    let node_id = endpoint.id();
    eprintln!("Iroh Node ID: {node_id}");

    endpoint.online().await;
    eprintln!("Iroh endpoint online and reachable");

    while let Some(incoming) = endpoint.accept().await {
        let mut accepting = match incoming.accept() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("incoming connection failed: {e}");
                continue;
            }
        };
        let conn = accepting.await?;
        let remote_id = conn.remote_id();
        eprintln!("New Iroh connection from: {remote_id}");
    }

    Ok(())
}
