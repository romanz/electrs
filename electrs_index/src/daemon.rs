use anyhow::{Context, Result};
use bitcoin::{hash_types::BlockHash, Amount, Transaction, Txid};
use bitcoincore_rpc::{Auth, Client, RpcApi};
use serde_json::json;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Deserialize, Serialize)]
pub struct BlockLocation {
    pub file: usize,
    pub data: usize,
    pub undo: Option<usize>, // genesis block has no undo
    pub prev: bitcoin::BlockHash,
}

#[derive(Debug, Deserialize, Serialize)]
struct GetBlockLocationsResult(Vec<BlockLocation>);

pub struct Daemon {
    client: Client,
    addr: SocketAddr,
    daemon_dir: PathBuf,
    blocks_dir: PathBuf,
}

impl Daemon {
    pub fn new(addr: SocketAddr, daemon_dir: &Path) -> Result<Self> {
        let cookie_path = daemon_dir.join(".cookie");
        if !cookie_path.exists() {
            bail!("missing cookie file: {:?}", cookie_path);
        }
        let auth = Auth::CookieFile(cookie_path);
        let client = Client::new(format!("http://{}", addr), auth)
            .with_context(|| format!("failed to connect {}", addr))?;
        let blocks_dir = daemon_dir.join("blocks");
        if !blocks_dir.exists() {
            bail!("missing blocks directory: {:?}", blocks_dir);
        }
        let blockchain_info = client
            .get_blockchain_info()
            .context("get_network_info failed")?;
        debug!("{:?}", blockchain_info);
        if blockchain_info.pruned {
            bail!("pruned node is not supported (use '-prune=0' bitcoind flag)")
        }
        let network_info = client
            .get_network_info()
            .context("get_network_info failed")?;
        debug!("{:?}", network_info);
        if network_info.version < 20_00_00 {
            bail!(
                "{} is not supported - please use bitcoind 0.20+",
                network_info.subversion,
            )
        }
        Ok(Daemon {
            client,
            addr,
            daemon_dir: daemon_dir.to_owned(),
            blocks_dir,
        })
    }

    pub fn reconnect(&self) -> Result<Self> {
        Self::new(self.addr, &self.daemon_dir)
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn broadcast(&self, tx: &Transaction) -> Result<Txid> {
        self.client
            .send_raw_transaction(tx)
            .context("broadcast failed")
    }

    pub fn get_best_block_hash(&self) -> Result<BlockHash> {
        self.client
            .get_best_block_hash()
            .context("get_best_block_hash failed")
    }

    pub fn wait_for_new_block(&self, timeout: Duration) -> Result<BlockHash> {
        Ok(self
            .client
            .wait_for_new_block(timeout.as_millis() as u64)?
            .hash)
    }

    pub fn get_block_locations(
        &self,
        hash: &BlockHash,
        nblocks: usize,
    ) -> Result<Vec<BlockLocation>> {
        self.client
            .call("getblocklocations", &[json!(hash), nblocks.into()])
            .context("get_block_locations failed")
    }

    pub fn get_relay_fee(&self) -> Result<Amount> {
        Ok(self
            .client
            .get_network_info()
            .context("get_network_info failed")?
            .relay_fee)
    }

    pub fn estimate_fee(&self, nblocks: u16) -> Result<Option<Amount>> {
        Ok(self
            .client
            .estimate_smart_fee(nblocks, None)
            .context("estimate_fee failed")?
            .fee_rate)
    }

    pub fn blk_file_path(&self, i: usize) -> PathBuf {
        self.blocks_dir.join(format!("blk{:05}.dat", i))
    }

    pub fn undo_file_path(&self, i: usize) -> PathBuf {
        self.blocks_dir.join(format!("rev{:05}.dat", i))
    }
}
