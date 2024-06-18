use anyhow::{bail, Result};
use bitcoin::{Transaction, Txid};

use std::sync::Arc;

use crate::daemon::Daemon;

/// Represents one of many possible ways of broadcasting transactions.
pub enum TxBroadcaster {
    BitcoinRPC(Arc<Daemon>),
    PushtxClear,
    PushtxTor,
    Script(String),
}

fn broadcast_with_script(script: &str, tx: &Transaction) -> Result<Txid> {
    let tx_hex = bitcoin::consensus::encode::serialize_hex(tx);
    let output = std::process::Command::new(script).arg(tx_hex).output()?;
    if !output.status.success() {
        bail!(
            "broadcasting TX with script '{}' failed with status code {}; stderr: {}",
            script,
            output
                .status
                .code()
                .map(|i| i.to_string())
                .unwrap_or_else(|| "<NULL>".to_string()),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(tx.compute_txid())
}

fn broadcast_with_pushtx(tx: &Transaction, opts: pushtx::Opts) -> Result<Txid> {
    let txs = vec![pushtx::Transaction::from_bytes(
        bitcoin::consensus::encode::serialize(tx),
    )?];
    let recv = pushtx::broadcast(txs, opts);

    loop {
        match recv.recv().unwrap() {
            pushtx::Info::Done(Ok(report)) => {
                if let Some(reason) = report.rejects.values().next() {
                    bail!("transaction rejected by peers: {reason}");
                }
                let txid = tx.compute_txid();
                info!("successful pushtx broadcast of {txid}");
                return Ok(txid);
            }
            pushtx::Info::Done(Err(e)) => bail!("failed to broadcast with pushtx: {e}"),
            pushtx::Info::ResolvingPeers => debug!("resolving pushtx peers from DNS seeds"),
            pushtx::Info::ResolvedPeers(n) => debug!("resolved {n} pushtx peers"),
            pushtx::Info::ConnectingToNetwork { tor_status } => match tor_status {
                None => {
                    debug!("connecting over clearnet to bitcoin P2P nodes for pushtx broadcast")
                }
                Some(addr) => debug!(
                    "connecting to bitcoin P2P nodes via Tor proxy {addr} for pushtx broadcast"
                ),
            },
            pushtx::Info::Broadcast { peer } => {
                debug!("successful broadcast to bitcoin node {peer}")
            }
        };
    }
}

impl TxBroadcaster {
    pub fn broadcast(&self, tx: &Transaction) -> Result<Txid> {
        match self {
            TxBroadcaster::BitcoinRPC(daemon) => daemon.broadcast(tx),
            TxBroadcaster::PushtxClear => broadcast_with_pushtx(
                tx,
                pushtx::Opts {
                    use_tor: pushtx::TorMode::No,
                    ..pushtx::Opts::default()
                },
            ),
            TxBroadcaster::PushtxTor => broadcast_with_pushtx(
                tx,
                pushtx::Opts {
                    use_tor: pushtx::TorMode::Must,
                    ..pushtx::Opts::default()
                },
            ),
            TxBroadcaster::Script(script) => broadcast_with_script(script, tx),
        }
    }
}
