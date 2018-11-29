use bincode;
use bitcoin::blockdata::block::Block;
use bitcoin::blockdata::script::Script;
use bitcoin::blockdata::transaction::Transaction;
use bitcoin::consensus::encode::{deserialize, serialize};
use bitcoin::util::hash::Sha256dHash;
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use rocksdb;

struct DBRow {
    key: Vec<u8>,
    value: Vec<u8>,
}

// use crypto::digest::Digest;
// use crypto::sha2::Sha256;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::Receiver;

// use daemon::Daemon;
// use metrics::{Counter, Gauge, HistogramOpts, HistogramTimer, HistogramVec, MetricOpts, Metrics};
// use signal::Waiter;
use daemon::Daemon;
use errors::*;
use util::{spawn_thread, Bytes, HeaderEntry, HeaderList, SyncChannel};

// use config::Config;

pub struct Indexer {
    // TODO: should be column families
    txns_db: rocksdb::DB,
    index_db: rocksdb::DB,

    added_blockhashes: HashSet<Sha256dHash>,
}

struct BlockEntry {
    block: Block,
    entry: HeaderEntry,
}

// TODO: &[Block] should be an iterator / a queue.
impl Indexer {
    pub fn open(path: &Path) -> Self {
        Indexer {
            txns_db: db_open(&path.join("txns")),
            index_db: db_open(&path.join("address")),
            added_blockhashes: HashSet::new(),
        }
    }

    pub fn update(&mut self, daemon: &Daemon, headers: HeaderList) -> Result<HeaderList> {
        let daemon = daemon.reconnect()?;
        let tip = daemon.getbestblockhash()?;
        let new_headers = headers.order(daemon.get_new_headers(&headers, &tip)?);
        info!("adding transactions from {} blocks", new_headers.len());
        for blocks in fetch_blocks(&daemon, &new_headers)? {
            self.add(&(blocks?));
        }
        info!("compacting txns DB");
        self.txns_db.compact_range(None, None);
        info!("indexing history from {} blocks", new_headers.len());
        for blocks in fetch_blocks(&daemon, &new_headers)? {
            self.index(&(blocks?));
        }

        let mut headers = headers;
        headers.apply(new_headers);
        assert_eq!(tip, *headers.tip());
        Ok(headers)
    }

    pub fn history(&self, script: &Script) -> HashMap<Sha256dHash, Transaction> {
        let scripthash = compute_script_hash(script.as_bytes());
        let rows = db_scan(&self.index_db, &TxHistoryRow::filter(&scripthash[..]));
        let txids = rows
            .into_iter()
            .map(|db_row| TxHistoryRow::from_row(&db_row))
            .map(|history_row| match history_row.key.txinfo {
                TxHistoryInfo::Funding(txid) => txid,
                TxHistoryInfo::Spending(txid) => txid,
            }).map(|txid| deserialize(&txid).expect("failed to deserialize Sha256dHash txid"))
            .collect();
        debug!("txids: {:?}", txids);
        self.lookup_txns(&txids)
    }

    fn add(&mut self, blocks: &[BlockEntry]) {
        // TODO: skip orphaned blocks?
        let rows = add_blocks(blocks);
        db_write(&self.txns_db, rows);

        self.added_blockhashes
            .extend(blocks.into_iter().map(|b| b.entry.hash()));
    }

    fn index(&mut self, blocks: &[BlockEntry]) {
        let previous_txns_map = self.lookup_txns(&get_previous_txids(blocks));
        for b in blocks {
            let blockhash = b.entry.hash();
            // TODO: replace by lookup into txns_db?
            if !self.added_blockhashes.contains(&blockhash) {
                panic!("cannot index block {} (missing from store)", blockhash);
            }
        }
        let rows = index_blocks(blocks, &previous_txns_map);
        db_write(&self.index_db, rows);
    }

    // TODO: can we pass txids as a "generic iterable"?
    fn lookup_txns(&self, txids: &BTreeSet<Sha256dHash>) -> HashMap<Sha256dHash, Transaction> {
        // TODO: in parallel
        txids
            .iter()
            .map(|txid| {
                let rows = db_scan(&self.txns_db, &TxRow::filter(&txid[..]));
                if rows.len() < 1 {
                    panic!("missing txid {} in txns DB", txid);
                }
                if rows.len() > 1 {
                    warn!("same tx {} was confirmed in >1 block", txid);
                }
                let row = TxRow::from_row(&rows[0]);
                let txn: Transaction =
                    deserialize(&row.value.rawtx).expect("failed to parse Transaction");
                assert_eq!(*txid, txn.txid());
                (*txid, txn)
            }).collect()
    }
}

type BlocksFetcher = Receiver<Result<Vec<BlockEntry>>>;

fn fetch_blocks(daemon: &Daemon, new_headers: &[HeaderEntry]) -> Result<BlocksFetcher> {
    new_headers.last().map(|tip| {
        info!("{:?} ({} new headers)", tip, new_headers.len());
    });
    let new_headers = new_headers.to_vec();
    let daemon = daemon.reconnect()?;
    let chan = SyncChannel::new(1);
    let sender = chan.sender();
    spawn_thread("bitcoind_fetcher", move || {
        for entry in new_headers {
            let msg = daemon.getblock(&entry.hash()).map(|b| {
                vec![BlockEntry {
                    block: b,
                    entry: entry.clone(),
                }]
            });
            sender
                .send(msg)
                .expect(&format!("failed to send fetched block {}", entry.hash()));
        }
    });
    Ok(chan.into_receiver())
}

fn db_open(path: &Path) -> rocksdb::DB {
    debug!("opening DB at {:?}", path);
    let mut db_opts = rocksdb::Options::default();
    db_opts.create_if_missing(true);
    db_opts.set_max_open_files(-1); // TODO: make sure to `ulimit -n` this process correctly
    db_opts.set_compaction_style(rocksdb::DBCompactionStyle::Level);
    db_opts.set_compression_type(rocksdb::DBCompressionType::Snappy);
    db_opts.set_target_file_size_base(256 << 20);
    db_opts.set_write_buffer_size(256 << 20);
    db_opts.set_disable_auto_compactions(true); // for initial bulk load

    // db_opts.set_advise_random_on_open(???);
    db_opts.set_compaction_readahead_size(1 << 20);

    // let mut block_opts = rocksdb::BlockBasedOptions::default();
    // block_opts.set_block_size(???);

    rocksdb::DB::open(&db_opts, path).expect("failed to open RocksDB")
}

fn db_scan(db: &rocksdb::DB, prefix: &[u8]) -> Vec<DBRow> {
    let mode = rocksdb::IteratorMode::From(prefix, rocksdb::Direction::Forward);
    db.iterator(mode)
        .take_while(|(key, _)| key.starts_with(prefix))
        .map(|(k, v)| DBRow {
            key: k.into_vec(),
            value: v.into_vec(),
        }).collect()
}

fn db_write(db: &rocksdb::DB, mut rows: Vec<DBRow>) {
    trace!("writing {} rows to {:?}", rows.len(), db);
    rows.sort_unstable_by(|a, b| a.key.cmp(&b.key));
    let mut batch = rocksdb::WriteBatch::default();
    for row in rows {
        batch.put(&row.key, &row.value).unwrap();
    }
    let mut opts = rocksdb::WriteOptions::new();
    opts.set_sync(false);
    opts.disable_wal(true);
    db.write_opt(batch, &opts).unwrap();
}

type FullHash = [u8; 32]; // serialized SHA256 result

#[derive(Serialize, Deserialize)]
struct TxRowKey {
    code: u8,
    txid: FullHash,
    blockhash: FullHash,
}

#[derive(Serialize, Deserialize)]
struct TxRowValue {
    rawtx: Bytes,
}

struct TxRow {
    key: TxRowKey,
    value: TxRowValue,
}

impl TxRow {
    fn new(txn: &Transaction, blockhash: &Sha256dHash) -> TxRow {
        let txid = txn.txid().into_bytes();
        TxRow {
            key: TxRowKey {
                code: b'T',
                txid,
                blockhash: blockhash.to_bytes(),
            },
            value: TxRowValue {
                rawtx: serialize(txn),
            },
        }
    }

    fn filter(prefix: &[u8]) -> Bytes {
        [b"T", prefix].concat()
    }

    fn to_row(&self) -> DBRow {
        let key = bincode::serialize(&self.key).unwrap();
        let value = bincode::serialize(&self.value).unwrap();
        // info!("tx id  = {}", hex::encode(&self.key.txid[..]));
        // info!("tx key = {}", hex::encode(&self.key.txid[..]));
        DBRow { key, value }
    }

    fn from_row(row: &DBRow) -> Self {
        let key = bincode::deserialize(&row.key).expect("failed to parse TxRowKey");
        let value = bincode::deserialize(&row.value).expect("failed to parse TxRowValue");
        TxRow { key, value }
    }
}

fn add_blocks(blocks: &[BlockEntry]) -> Vec<DBRow> {
    // persist individual transactions:
    //      T{txid}{blockhash} → {rawtx}
    //      O{txid}{index} → {txout}
    // persist block headers' and txids' rows:
    //      B{blockhash} → {header}
    //      X{blockhash} → {txid1}...{txidN}
    let mut rows = vec![];
    for b in blocks {
        let blockhash = b.entry.hash();
        for tx in &b.block.txdata {
            rows.push(TxRow::new(tx, &blockhash).to_row());
            // TODO: add O, B, X rows as well.
        }
    }
    rows
}

fn get_previous_txids(blocks: &[BlockEntry]) -> BTreeSet<Sha256dHash> {
    blocks
        .iter()
        .flat_map(|b| {
            b.block.txdata.iter().flat_map(|tx| {
                tx.input.iter().filter_map(|txin| {
                    if txin.previous_output.is_null() {
                        None
                    } else {
                        Some(txin.previous_output.txid)
                    }
                })
            })
        }).collect()
}

fn index_blocks(
    blocks: &[BlockEntry],
    previous_txns_map: &HashMap<Sha256dHash, Transaction>,
) -> Vec<DBRow> {
    let mut rows = vec![];
    for b in blocks {
        for tx in &b.block.txdata {
            let height = b.entry.height() as u32;
            index_transaction(tx, height, previous_txns_map, &mut rows);
        }
    }
    rows
}

// TODO: return an iterator?
fn index_transaction(
    tx: &Transaction,
    confirmed_height: u32,
    previous_txns_map: &HashMap<Sha256dHash, Transaction>,
    rows: &mut Vec<DBRow>,
) {
    // persist history index:
    //      H{funding-scripthash}{funding-height}F{funding-txid} → ""
    //      H{funding-scripthash}{spending-height}S{spending-txid}{funding-txid} → ""
    // persist "edges" for fast is-this-TXO-spent check
    //      S{funding-txid:vout}{spending-txid} → ""
    let txid = tx.txid().into_bytes();
    for txo in &tx.output {
        let history = TxHistoryRow::new(
            &txo.script_pubkey,
            confirmed_height,
            TxHistoryInfo::Funding(txid),
        );
        rows.push(history.to_row())
    }
    for txi in &tx.input {
        if txi.previous_output.is_null() {
            continue;
        }
        let prev_txid = txi.previous_output.txid;
        let prev_txn = previous_txns_map
            .get(&prev_txid)
            .expect(&format!("missing previous txn {:?}", prev_txid));
        let vout = txi.previous_output.vout as usize;
        let out = prev_txn
            .output
            .get(vout)
            .expect(&format!("missing output #{} at txn {}", vout, prev_txid));
        let history = TxHistoryRow::new(
            &out.script_pubkey,
            confirmed_height,
            TxHistoryInfo::Spending(txid),
        );
        rows.push(history.to_row())

        // TODO: add S row as well
    }
}

fn compute_script_hash(script: &[u8]) -> FullHash {
    let mut hash = FullHash::default();
    let mut sha2 = Sha256::new();
    sha2.input(script);
    sha2.result(&mut hash);
    hash
}

#[derive(Serialize, Deserialize)]
enum TxHistoryInfo {
    Funding(FullHash),  // funding txid
    Spending(FullHash), // spending txid
}

#[derive(Serialize, Deserialize)]
struct TxHistoryKey {
    code: u8,
    scripthash: FullHash,
    confirmed_height: u32,
    txinfo: TxHistoryInfo,
}

struct TxHistoryRow {
    key: TxHistoryKey,
}

impl TxHistoryRow {
    fn new(script: &Script, confirmed_height: u32, txinfo: TxHistoryInfo) -> Self {
        let key = TxHistoryKey {
            code: b'H',
            scripthash: compute_script_hash(&script[..]),
            confirmed_height,
            txinfo,
        };
        TxHistoryRow { key }
    }

    fn filter(scripthash_prefix: &[u8]) -> Bytes {
        [b"H", scripthash_prefix].concat()
    }

    fn to_row(&self) -> DBRow {
        DBRow {
            key: bincode::serialize(&self.key).unwrap(),
            value: vec![],
        }
    }

    fn from_row(row: &DBRow) -> Self {
        let key = bincode::deserialize(&row.key).expect("failed to deserialize TxHistoryKey");
        TxHistoryRow { key }
    }
}

// #[derive(Serialize, Deserialize)]
// struct TxEdgeKey {
//     code: u8,
//     funding_txid: FullHash,
//     funding_vout: u16,
//     spending_txid: FullHash,
//     spending_vin: u16,
// }
