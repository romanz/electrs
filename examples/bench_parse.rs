extern crate electrs;

#[macro_use]
extern crate log;

extern crate error_chain;

use std::path::{Path, PathBuf};
use std::sync::{
    mpsc::{Receiver, SyncSender}, Arc, Mutex,
};

use electrs::{
    bulk::Parser, config::Config, daemon::Daemon, errors::*, metrics::Metrics,
    store::{DBStore, Row, StoreOptions}, util::{spawn_thread, SyncChannel},
};

use error_chain::ChainedError;

type ReadResult = Result<Vec<u8>>;
type IndexResult = Result<Vec<Row>>;
type JoinHandle = std::thread::JoinHandle<()>;

fn start_reader(blk_files: Vec<PathBuf>, parser: Arc<Parser>) -> Arc<Mutex<Receiver<ReadResult>>> {
    let chan = SyncChannel::new(0);
    let tx = chan.sender();
    spawn_thread("bulk_read", move || {
        for path in blk_files {
            tx.send(parser.read_blkfile(&path))
                .expect("failed to send blob");
        }
    });
    Arc::new(Mutex::new(chan.into_receiver()))
}

fn start_indexer(
    rx: Arc<Mutex<Receiver<ReadResult>>>,
    tx: SyncSender<IndexResult>,
    parser: Arc<Parser>,
) -> JoinHandle {
    spawn_thread("bulk_index", move || loop {
        let msg = match rx.lock().unwrap().recv() {
            Ok(msg) => msg,
            Err(_) => {
                debug!("no more blocks to index");
                break;
            }
        };
        tx.send(match msg {
            Ok(blob) => parser.index_blkfile(blob),
            Err(err) => Err(err),
        }).expect("failed to send indexed rows");
    })
}

fn run(config: Config) -> Result<()> {
    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();

    let daemon = Daemon::new(
        &config.daemon_dir,
        &config.cookie,
        config.network_type,
        &metrics,
    )?;
    let store = DBStore::open(
        Path::new("/opt/tmp/test-db"),
        StoreOptions { bulk_import: true },
    );
    let blk_files = daemon.list_blk_files()?;
    let parser = Arc::new(Parser::new(&daemon, &metrics)?);
    let reader = start_reader(blk_files, parser.clone());
    let indexed = SyncChannel::new(0);
    let indexers: Vec<JoinHandle> = (0..2)
        .map(|_| start_indexer(reader.clone(), indexed.sender(), parser.clone()))
        .collect();

    for (i, rows) in indexed.into_receiver().into_iter().enumerate() {
        let path = format!("./sstables/{:05}.sst", i);
        store.sstable().build(Path::new(&path), rows?);
        debug!("{} built", path);
    }
    indexers.into_iter().for_each(|i| i.join().expect("indexer failed"));

    Ok(())
}

fn main() {
    if let Err(e) = run(Config::from_args()) {
        error!("{}", e.display_chain());
    }
}
