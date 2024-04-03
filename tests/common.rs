use std::sync::{Arc, Once, RwLock};
use std::{env, net};

use stderrlog::StdErrLog;
use tempfile::TempDir;

use serde_json::json;
#[cfg(feature = "liquid")]
use serde_json::Value;

#[cfg(not(feature = "liquid"))]
use bitcoind::{self as noded, BitcoinD as NodeD};
#[cfg(feature = "liquid")]
use elementsd::{self as noded, ElementsD as NodeD};

use noded::bitcoincore_rpc::{self, RpcApi};

use electrs::{
    chain::{Address, BlockHash, Network, Txid},
    config::Config,
    daemon::Daemon,
    electrum::RPC as ElectrumRPC,
    metrics::Metrics,
    new_index::{ChainQuery, FetchFrom, Indexer, Mempool, Query, Store},
    rest,
    signal::Waiter,
};

pub struct TestRunner {
    config: Arc<Config>,
    /// bitcoind::BitcoinD or an elementsd::ElementsD in liquid mode
    node: NodeD,
    _electrsdb: TempDir, // rm'd when dropped
    indexer: Indexer,
    query: Arc<Query>,
    daemon: Arc<Daemon>,
    mempool: Arc<RwLock<Mempool>>,
    metrics: Metrics,
}

impl TestRunner {
    pub fn new() -> Result<TestRunner> {
        let log = init_log();

        // Setup the bitcoind/elementsd config
        let mut node_conf = noded::Conf::default();
        {
            #[cfg(not(feature = "liquid"))]
            let node_conf = &mut node_conf;
            #[cfg(feature = "liquid")]
            let node_conf = &mut node_conf.0;

            #[cfg(feature = "liquid")]
            node_conf.args.push("-anyonecanspendaremine=1");

            node_conf.view_stdout = true;
        }

        // Setup node
        let node = NodeD::with_conf(noded::downloaded_exe_path().unwrap(), &node_conf).unwrap();

        #[cfg(not(feature = "liquid"))]
        let (node_client, params) = (&node.client, &node.params);
        #[cfg(feature = "liquid")]
        let (node_client, params) = (node.client(), &node.params());

        log::info!("node params: {:?}", params);

        generate(node_client, 101).chain_err(|| "failed initializing blocks")?;

        // Needed to claim the initialfreecoins as our own
        // See https://github.com/ElementsProject/elements/issues/956
        #[cfg(feature = "liquid")]
        node_client.call::<Value>("rescanblockchain", &[])?;

        #[cfg(not(feature = "liquid"))]
        let network_type = Network::Regtest;
        #[cfg(feature = "liquid")]
        let network_type = Network::LiquidRegtest;

        let mut daemon_subdir = params.cookie_file.clone();
        // drop `.cookie` filename, leaving just the network subdirectory
        daemon_subdir.pop();

        let electrsdb = tempfile::tempdir().unwrap();

        let config = Arc::new(Config {
            log,
            network_type,
            db_path: electrsdb.path().to_path_buf(),
            daemon_dir: daemon_subdir.clone(),
            blocks_dir: daemon_subdir.join("blocks"),
            daemon_rpc_addr: params.rpc_socket.into(),
            cookie: None,
            electrum_rpc_addr: rand_available_addr(),
            http_addr: rand_available_addr(),
            http_socket_file: None, // XXX test with socket file or tcp?
            monitoring_addr: rand_available_addr(),
            jsonrpc_import: false,
            light_mode: false,
            address_search: true,
            index_unspendables: false,
            cors: None,
            precache_scripts: None,
            utxos_limit: 100,
            electrum_txs_limit: 100,
            electrum_banner: "".into(),
            electrum_rpc_logging: None,

            #[cfg(feature = "liquid")]
            asset_db_path: None, // XXX
            #[cfg(feature = "liquid")]
            parent_network: bitcoin::Network::Regtest,
            //#[cfg(feature = "electrum-discovery")]
            //electrum_public_hosts: Option<crate::electrum::ServerHosts>,
            //#[cfg(feature = "electrum-discovery")]
            //electrum_announce: bool,
            //#[cfg(feature = "electrum-discovery")]
            //tor_proxy: Option<std::net::SocketAddr>,
        });

        let signal = Waiter::start();
        let metrics = Metrics::new(rand_available_addr());
        metrics.start();

        let daemon = Arc::new(Daemon::new(
            &config.daemon_dir,
            &config.blocks_dir,
            config.daemon_rpc_addr,
            config.cookie_getter(),
            config.network_type,
            signal.clone(),
            &metrics,
        )?);

        let store = Arc::new(Store::open(&config.db_path.join("newindex"), &config));

        let fetch_from = if !env::var("JSONRPC_IMPORT").is_ok() && !cfg!(feature = "liquid") {
            // run the initial indexing from the blk files then switch to using the jsonrpc,
            // similarly to how electrs is typically used.
            FetchFrom::BlkFiles
        } else {
            // when JSONRPC_IMPORT is set, use the jsonrpc for the initial indexing too.
            // this runs faster on small regtest chains and can be useful for quicker local development iteration.
            // this is also used on liquid regtest, which currently fails to parse the BlkFiles due to the magic bytes
            FetchFrom::Bitcoind
        };

        let mut indexer = Indexer::open(Arc::clone(&store), fetch_from, &config, &metrics);
        indexer.update(&daemon)?;
        indexer.fetch_from(FetchFrom::Bitcoind);

        let chain = Arc::new(ChainQuery::new(
            Arc::clone(&store),
            Arc::clone(&daemon),
            &config,
            &metrics,
        ));

        let mempool = Arc::new(RwLock::new(Mempool::new(
            Arc::clone(&chain),
            &metrics,
            Arc::clone(&config),
        )));
        Mempool::update(&mempool, &daemon)?;

        let query = Arc::new(Query::new(
            Arc::clone(&chain),
            Arc::clone(&mempool),
            Arc::clone(&daemon),
            Arc::clone(&config),
            #[cfg(feature = "liquid")]
            None, // TODO
        ));

        Ok(TestRunner {
            config,
            node,
            _electrsdb: electrsdb,
            indexer,
            query,
            daemon,
            mempool,
            metrics,
        })
    }

    pub fn node_client(&self) -> &bitcoincore_rpc::Client {
        #[cfg(not(feature = "liquid"))]
        return &self.node.client;
        #[cfg(feature = "liquid")]
        return &self.node.client();
    }

    pub fn sync(&mut self) -> Result<()> {
        self.indexer.update(&self.daemon)?;
        Mempool::update(&self.mempool, &self.daemon)?;
        // force an update for the mempool stats, which are normally cached
        self.mempool.write().unwrap().update_backlog_stats();
        Ok(())
    }

    pub fn mine(&mut self) -> Result<BlockHash> {
        let mut generated = generate(self.node_client(), 1)?;
        self.sync()?;
        Ok(generated.remove(0))
    }

    pub fn send(&mut self, addr: &Address, amount: bitcoin::Amount) -> Result<Txid> {
        // Must use raw call() because send_to_address() expects a bitcoin::Address and not an elements::Address
        let txid = self.node_client().call(
            "sendtoaddress",
            &[addr.to_string().into(), json!(amount.to_btc())],
        )?;
        self.sync()?;
        Ok(txid)
    }

    #[cfg(feature = "liquid")]
    pub fn send_asset(
        &mut self,
        addr: &Address,
        amount: bitcoin::Amount,
        assetid: elements::AssetId,
    ) -> Result<Txid> {
        let txid = self.node_client().call(
            "sendtoaddress",
            &[
                addr.to_string().into(),
                json!(amount.to_btc()),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                json!(assetid),
            ],
        )?;
        self.sync()?;
        Ok(txid)
    }

    /// Generate and return a new address.
    /// Returns the unconfidential address in Liquid mode, to make it interchangeable with Bitcoin addresses in tests.
    pub fn newaddress(&self) -> Result<Address> {
        #[cfg(not(feature = "liquid"))]
        return Ok(raw_new_address(self.node_client())?);

        #[cfg(feature = "liquid")]
        return Ok(self.ct_newaddress()?.1);
    }
    /// Generate a new address, returning both the confidential and non-confidential versions
    #[cfg(feature = "liquid")]
    pub fn ct_newaddress(&self) -> Result<(Address, Address)> {
        let client = self.node_client();
        let c_addr = raw_new_address(client)?;
        let mut info = client.call::<Value>("getaddressinfo", &[c_addr.to_string().into()])?;
        let uc_addr = serde_json::from_value(info["unconfidential"].take())?;
        Ok((c_addr, uc_addr))
    }
}

pub fn init_rest_tester() -> Result<(rest::Handle, net::SocketAddr, TestRunner)> {
    let tester = TestRunner::new()?;
    let rest_server = rest::start(Arc::clone(&tester.config), Arc::clone(&tester.query));
    log::info!("REST server running on {}", tester.config.http_addr);
    Ok((rest_server, tester.config.http_addr, tester))
}
pub fn init_electrum_tester() -> Result<(ElectrumRPC, net::SocketAddr, TestRunner)> {
    let tester = TestRunner::new()?;
    let electrum_server = ElectrumRPC::start(
        Arc::clone(&tester.config),
        Arc::clone(&tester.query),
        &tester.metrics,
    );
    log::info!(
        "Electrum server running on {}",
        tester.config.electrum_rpc_addr
    );
    Ok((electrum_server, tester.config.electrum_rpc_addr, tester))
}

#[cfg(not(feature = "liquid"))]
fn raw_new_address(
    client: &bitcoincore_rpc::Client,
) -> bitcoincore_rpc::Result<Address<bitcoin::address::NetworkChecked>> {
    Ok(client.get_new_address(None, None)?.assume_checked())
}

// Returns the confidential address
#[cfg(feature = "liquid")]
fn raw_new_address(client: &bitcoincore_rpc::Client) -> bitcoincore_rpc::Result<Address> {
    // Must use raw call() because get_new_address() returns a bitcoin::Address and not an elements::Address
    Ok(client.call::<Address>("getnewaddress", &[])?)
}

fn generate(
    client: &bitcoincore_rpc::Client,
    num_blocks: u32,
) -> bitcoincore_rpc::Result<Vec<BlockHash>> {
    let addr = raw_new_address(client)?;
    client.call(
        "generatetoaddress",
        &[num_blocks.into(), addr.to_string().into()],
    )
}

fn init_log() -> StdErrLog {
    static ONCE: Once = Once::new();
    let mut log = stderrlog::new();
    log.verbosity(4);
    // log.timestamp(stderrlog::Timestamp::Millisecond        );
    ONCE.call_once(|| log.init().expect("logging initialization failed"));
    log
}

fn rand_available_addr() -> net::SocketAddr {
    // note this has a potential but unlikely race condition, if the port is grabbed before the caller binds it
    let socket = net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.local_addr().unwrap()
}

error_chain::error_chain! {
    types {
        Error, ErrorKind, ResultExt, Result;
    }

    errors {
        Electrs(e: electrs::errors::Error) {
            description("Electrs error")
            display("Electrs error: {:?}", e)
        }

        BitcoindRpc(e: bitcoind::bitcoincore_rpc::Error) {
            description("Bitcoind RPC error")
            display("Bitcoind RPC error: {:?}", e)
        }

        ElectrumD(e: electrumd::Error) {
            description("Electrum wallet RPC error")
            display("Electrum wallet RPC error: {:?}", e)
        }

        Io(e: std::io::Error) {
            description("IO error")
            display("IO error: {:?}", e)
        }
        Ureq(e: ureq::Error) {
            description("ureq error")
            display("ureq error: {:?}", e)
        }
        Json(e: serde_json::Error) {
            description("JSON error")
            display("JSON error: {:?}", e)
        }
    }
}

impl From<electrs::errors::Error> for Error {
    fn from(e: electrs::errors::Error) -> Self {
        Error::from(ErrorKind::Electrs(e))
    }
}
impl From<bitcoind::bitcoincore_rpc::Error> for Error {
    fn from(e: bitcoind::bitcoincore_rpc::Error) -> Self {
        Error::from(ErrorKind::BitcoindRpc(e))
    }
}
impl From<electrumd::Error> for Error {
    fn from(e: electrumd::Error) -> Self {
        Error::from(ErrorKind::ElectrumD(e))
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::from(ErrorKind::Io(e))
    }
}
impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        Error::from(ErrorKind::Ureq(e))
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::from(ErrorKind::Json(e))
    }
}
