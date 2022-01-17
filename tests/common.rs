use std::sync::{Arc, Once, RwLock};
use std::{env, net};

use stderrlog::StdErrLog;

use bitcoind::{
    bitcoincore_rpc::{self, RpcApi},
    BitcoinD,
};

use electrs::{
    chain::Network,
    config::{self, Config},
    daemon::Daemon,
    electrum::RPC as ElectrumRPC,
    metrics::Metrics,
    new_index::{ChainQuery, FetchFrom, Indexer, Mempool, Query, Store},
    rest,
    signal::Waiter,
};
use tempfile::TempDir;

pub fn init_server() -> Result<TestRunner> {
    let log = init_log();

    let mut bitcoind_conf = bitcoind::Conf::default();
    bitcoind_conf.view_stdout = true;

    let bitcoind =
        BitcoinD::with_conf(bitcoind::downloaded_exe_path().unwrap(), &bitcoind_conf).unwrap();

    init_node(&bitcoind.client).chain_err(|| "failed initializing node")?;

    log::info!("bitcoind: {:?}", bitcoind.params);

    #[cfg(not(feature = "liquid"))]
    let network_type = Network::Regtest;
    #[cfg(feature = "liquid")]
    let network_type = Network::LiquidRegtest;

    let daemon_subdir = bitcoind
        .params
        .datadir
        .join(config::get_network_subdir(network_type).unwrap());

    let electrsdb = tempfile::tempdir().unwrap();

    let config = Arc::new(Config {
        log,
        network_type,
        db_path: electrsdb.path().to_path_buf(),
        daemon_dir: daemon_subdir.clone(),
        blocks_dir: daemon_subdir.join("blocks"),
        daemon_rpc_addr: bitcoind.params.rpc_socket.into(),
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

        #[cfg(feature = "liquid")]
        asset_db_path: None, // XXX
        #[cfg(feature = "liquid")]
        parent_network: electrs::chain::BNetwork::Regtest,
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


    let fetch_from = if !env::var("JSONRPC_IMPORT").is_ok() {
        // run the initial indexing from the blk files then switch to using the jsonrpc,
        // similarly to how electrs is typically used.
        FetchFrom::BlkFiles
    } else {
        // when JSONRPC_IMPORT is set, use the jsonrpc for the initial indexing too.
        // this runs faster on small regtest chains and can be useful for quicker local development iteration.
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
    mempool.write().unwrap().update(&daemon)?;

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
        bitcoind,
        _electrsdb: electrsdb,
        indexer,
        query,
        daemon,
        mempool,
        metrics,
    })
}

pub struct TestRunner {
    config: Arc<Config>,
    bitcoind: BitcoinD,
    _electrsdb: TempDir, // rm'd when dropped
    indexer: Indexer,
    query: Arc<Query>,
    daemon: Arc<Daemon>,
    mempool: Arc<RwLock<Mempool>>,
    metrics: Metrics,
}

impl TestRunner {
    pub fn bitcoind(&self) -> &bitcoincore_rpc::Client {
        &self.bitcoind.client
    }

    pub fn sync(&mut self) -> Result<()> {
        self.indexer.update(&self.daemon)?;
        self.mempool.write().unwrap().update(&self.daemon)?;
        Ok(())
    }
}

pub fn init_rest_server() -> Result<(rest::Handle, net::SocketAddr, TestRunner)> {
    let tester = init_server()?;
    let rest_server = rest::start(Arc::clone(&tester.config), Arc::clone(&tester.query));
    log::info!("REST server running on {}", tester.config.http_addr);
    Ok((rest_server, tester.config.http_addr, tester))
}
pub fn init_electrum_server() -> Result<(ElectrumRPC, net::SocketAddr, TestRunner)> {
    let tester = init_server()?;
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

fn init_node(client: &bitcoincore_rpc::Client) -> bitcoincore_rpc::Result<()> {
    let addr = client.get_new_address(None, None)?;
    client.generate_to_address(101, &addr)?;
    Ok(())
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
