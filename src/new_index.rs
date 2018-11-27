use bincode;
use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::blockdata::script::Script;
use bitcoin::blockdata::transaction::{OutPoint, Transaction, TxIn, TxOut};
use bitcoin::consensus::encode::{deserialize, serialize};
use bitcoin::util::hash::BitcoinHash;
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
// use std::iter::FromIterator;
// use std::sync::RwLock;
use std::path::{Path, PathBuf};

// use daemon::Daemon;
// use metrics::{Counter, Gauge, HistogramOpts, HistogramTimer, HistogramVec, MetricOpts, Metrics};
// use signal::Waiter;
use util::{Bytes, HashPrefix};

// use config::Config;
// use errors::*;

pub struct Indexer {
    // TODO: should be column families
    txns_db: rocksdb::DB,
    index_db: rocksdb::DB,

    added_blockhashes: HashSet<Sha256dHash>,
}

// TODO: &[Block] should be an iterator / a queue.
impl Indexer {
    pub fn new(path: &Path) -> Self {
        Indexer {
            txns_db: db_open(&path.join("txns")),
            index_db: db_open(&path.join("address")),
            added_blockhashes: HashSet::new(),
        }
    }

    pub fn add(&mut self, blocks: &[Block]) {
        let rows = add_blocks(blocks);
        db_write(&self.txns_db, rows);

        self.added_blockhashes
            .extend(blocks.into_iter().map(|block| block.bitcoin_hash()));
    }

    pub fn index(&mut self, blocks: &[Block]) {
        let previous_txns_map = self.lookup_txns(get_previous_txids(&blocks));
        let rows = index_blocks(&blocks, &previous_txns_map);
        db_write(&self.index_db, rows);
    }

    // TODO: can we pass txids as a "generic iterable"?
    fn lookup_txns(&self, txids: BTreeSet<Sha256dHash>) -> HashMap<Sha256dHash, Transaction> {
        // TODO: in parallel
        txids
            .into_iter()
            .map(|txid| {
                let rows = db_scan(&self.txns_db, &TxRow::filter(&txid));
                if rows.len() < 1 {
                    panic!("missing txid {}", txid);
                }
                if rows.len() > 1 {
                    warn!("same tx {} was confirmed in >1 block", txid);
                }
                let row = TxRow::from_row(&rows[0]);
                let txn: Transaction =
                    deserialize(&row.value.rawtx).expect("failed to parse Transaction");
                (txid, txn)
            }).collect()
    }
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

#[derive(Serialize, Deserialize)]
struct TxRowKey {
    code: u8,
    txid: Sha256dHash,
    blockhash: Sha256dHash,
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
    fn new(txn: &Transaction, blockhash: Sha256dHash) -> TxRow {
        TxRow {
            key: TxRowKey {
                code: b'T',
                txid: txn.txid(),
                blockhash,
            },
            value: TxRowValue {
                rawtx: serialize(txn),
            },
        }
    }

    fn filter(txid: &Sha256dHash) -> Bytes {
        [b"T", &txid[..]].concat()
    }

    fn filter_by_prefix(prefix: &[u8]) -> Bytes {
        [b"T", prefix].concat()
    }

    fn to_row(&self) -> DBRow {
        DBRow {
            key: bincode::serialize(&self.key).unwrap(),
            value: bincode::serialize(&self.value).unwrap(),
        }
    }

    fn from_row(row: &DBRow) -> TxRow {
        TxRow {
            key: bincode::deserialize(&row.key).expect("failed to parse TxRowKey"),
            value: bincode::deserialize(&row.value).expect("failed to parse TxRowValue"),
        }
    }
}

fn add_blocks(blocks: &[Block]) -> Vec<DBRow> {
    // persist individual transactions:
    //      T{txid}{blockhash} → {rawtx}
    //      O{txid}{index} → {txout}
    // persist block headers' and txids' rows:
    //      B{blockhash} → {header}
    //      X{blockhash} → {txid1}...{txidN}
    blocks
        .iter()
        .flat_map(|block| {
            let blockhash = block.bitcoin_hash();
            block
                .txdata
                .iter()
                .map(move |tx| TxRow::new(tx, blockhash).to_row())
            // TODO: add O, B, X rows as well.
        }).collect()
}

fn get_previous_txids(blocks: &[Block]) -> BTreeSet<Sha256dHash> {
    blocks
        .iter()
        .flat_map(|block| {
            block.txdata.iter().flat_map(|tx| {
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
    blocks: &[Block],
    previous_txns_map: &HashMap<Sha256dHash, Transaction>,
) -> Vec<DBRow> {
    blocks
        .iter()
        .flat_map(|block| {
            block
                .txdata
                .iter()
                .flat_map(|tx| index_transaction(tx, previous_txns_map))
        }).collect()
}

fn index_transaction(
    tx: &Transaction,
    previous_txns_map: &HashMap<Sha256dHash, Transaction>,
) -> Vec<DBRow> {
    // persist history index:
    //      H{funding-scripthash}{funding-height}F{funding-txid} → ""
    //      H{funding-scripthash}{spending-height}S{spending-txid}{funding-txid} → ""
    // persist "edges" for fast is-this-TXO-spent check
    //      S{funding-txid:vout}{spending-txid} → ""
    let tx_height = 0; // TODO!!!
    let mut rows = vec![];
    let txid = tx.txid();
    for txin in &tx.input {
        if txin.previous_output.is_null() {
            continue;
        }
        let prev_tx = previous_txns_map
            .get(&txin.previous_output.txid)
            .expect(&format!(
                "failed to find prevout {:?}",
                txin.previous_output
            ));
        let prev_txid = prev_tx.txid();
        let vout = txin.previous_output.vout as usize;
        let out = prev_tx
            .output
            .get(vout)
            .expect(&format!("missing output #{} at txn {}", vout, prev_txid));
        let info = TxSpendingInfo { txid, prev_txid };
        let history =
            TxHistoryRow::new(&out.script_pubkey, tx_height, TxHistoryInfo::Spending(info));
        rows.push(history.to_row())

        // TODO: add S row as well
    }
    for out in &tx.output {
        let info = TxFundingInfo { txid };
        let history =
            TxHistoryRow::new(&out.script_pubkey, tx_height, TxHistoryInfo::Funding(info));
        rows.push(history.to_row())
    }
    rows
}

type ScriptHash = [u8; 32]; // single SHA256 over ScriptPubKey

fn compute_script_hash(script: &[u8]) -> ScriptHash {
    let mut hash = ScriptHash::default();
    let mut sha2 = Sha256::new();
    sha2.input(script);
    sha2.result(&mut hash);
    hash
}

#[derive(Serialize, Deserialize)]
struct TxFundingInfo {
    txid: Sha256dHash,
}

#[derive(Serialize, Deserialize)]
struct TxSpendingInfo {
    txid: Sha256dHash,
    prev_txid: Sha256dHash,
}

#[derive(Serialize, Deserialize)]
enum TxHistoryInfo {
    Funding(TxFundingInfo),
    Spending(TxSpendingInfo),
}

#[derive(Serialize, Deserialize)]
struct TxHistoryKey {
    code: u8,
    scripthash: ScriptHash,
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

    fn to_row(&self) -> DBRow {
        DBRow {
            key: bincode::serialize(&self.key).unwrap(),
            value: vec![],
        }
    }
}

// #[derive(Serialize, Deserialize)]
// struct TxEdgeKey {
//     code: u8,
//     funding_txid: Sha256dHash,
//     funding_vout: u16,
//     spending_txid: Sha256dHash,
//     spending_vin: u16,
// }
