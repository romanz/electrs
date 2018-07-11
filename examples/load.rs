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
    store::{DBStore, Row, StoreOptions, WriteStore}, util::{spawn_thread, SyncChannel},
};

use error_chain::ChainedError;

type JoinHandle = std::thread::JoinHandle<Result<()>>;
type BlobReceiver = Arc<Mutex<Receiver<Vec<u8>>>>;

fn start_reader(blk_files: Vec<PathBuf>, parser: Arc<Parser>) -> (BlobReceiver, JoinHandle) {
    let chan = SyncChannel::new(0);
    let blobs = chan.sender();
    let handle = spawn_thread("bulk_read", move || -> Result<()> {
        for path in blk_files {
            blobs
                .send(parser.read_blkfile(&path)?)
                .expect("failed to send blk*.dat contents");
        }
        Ok(())
    });
    (Arc::new(Mutex::new(chan.into_receiver())), handle)
}

fn start_indexer(
    blobs: BlobReceiver,
    parser: Arc<Parser>,
    rows: SyncSender<Vec<Row>>,
) -> JoinHandle {
    spawn_thread("bulk_index", move || -> Result<()> {
        loop {
            let msg = blobs.lock().unwrap().recv();
            if let Ok(blob) = msg {
                rows.send(parser.index_blkfile(blob)?)
                    .expect("failed to send indexed rows")
            } else {
                debug!("no more blocks to index");
                break;
            }
        }
        Ok(())
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
    let store = DBStore::open(Path::new("./test-db"), StoreOptions { bulk_import: true });
    let blk_files = daemon.list_blk_files()?;
    let parser = Arc::new(Parser::new(&daemon, &metrics)?);
    let (blobs, reader) = start_reader(blk_files, parser.clone());
    let rows_chan = SyncChannel::new(0);
    let indexers: Vec<JoinHandle> = (0..2)
        .map(|_| start_indexer(blobs.clone(), parser.clone(), rows_chan.sender()))
        .collect();
    spawn_thread("bulk_writer", move || {
        for rows in rows_chan.into_receiver() {
            debug!("indexed {:.2}M rows", rows.len() as f32 / 1e6);
            store.write(rows);
        }
        reader
            .join()
            .expect("reader panicked")
            .expect("reader failed");

        indexers.into_iter().for_each(|i| {
            i.join()
                .expect("indexer panicked")
                .expect("indexing failed")
        });
        store.write(vec![parser.last_indexed_row()]);
        store.flush();
        store.compact(); // will take a while.
    }).join()
        .expect("writer panicked");
    Ok(())
}

fn main() {
    if let Err(e) = run(Config::from_args()) {
        error!("{}", e.display_chain());
    }
}
