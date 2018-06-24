extern crate bitcoin;
extern crate electrs;

#[macro_use]
extern crate log;
#[macro_use]
extern crate error_chain;

use electrs::{config::Config,
              daemon::Daemon,
              errors::*,
              metrics::Metrics,
              parse::Parser,
              signal::Waiter,
              store::{DBStore, StoreOptions, WriteStore},
              util::{HeaderEntry, HeaderList}};

use error_chain::ChainedError;

fn run(config: Config) -> Result<()> {
    let signal = Waiter::new();
    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();

    let daemon = Daemon::new(config.network_type, &metrics)?;
    let store = DBStore::open("./test-db", StoreOptions { bulk_import: true });

    let tip = daemon.getbestblockhash()?;
    let new_headers: Vec<HeaderEntry> = {
        let indexed_headers = HeaderList::empty();
        indexed_headers.order(daemon.get_new_headers(&indexed_headers, &tip)?)
    };
    new_headers.last().map(|tip| {
        info!("{:?} ({} left to index)", tip, new_headers.len());
    });

    let chan = Parser::new(&daemon, &metrics)?.start(new_headers);
    for rows in chan.iter() {
        if let Some(sig) = signal.poll() {
            bail!("indexing interrupted by SIG{:?}", sig);
        }
        store.write(rows?);
    }
    debug!("done");
    Ok(())
}

fn main() {
    if let Err(e) = run(Config::from_args()) {
        eprintln!("{}", e.display_chain());
    }
}
