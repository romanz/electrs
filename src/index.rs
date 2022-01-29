use anyhow::{Context, Result};
use bitcoin::{
    consensus::{deserialize, serialize, Decodable},
    BlockHeader, OutPoint, Transaction, Txid, VarInt,
};

use std::convert::TryFrom;
use std::io::{Read, Seek, SeekFrom};

use crate::{
    chain::Chain,
    daemon::{BlockHashPosition, Daemon},
    db::{DBStore, Row, WriteBatch},
    metrics::{self, Gauge, Histogram, Metrics},
    signals::ExitFlag,
    types::{
        FilePosition, HashPrefixRow, HeaderRow, ScriptHash, ScriptHashRow, SpendingPrefixRow,
        TxidRow,
    },
};

#[derive(Clone)]
struct Stats {
    update_duration: Histogram,
    update_size: Histogram,
    height: Gauge,
    db_properties: Gauge,
}

impl Stats {
    fn new(metrics: &Metrics) -> Self {
        Self {
            update_duration: metrics.histogram_vec(
                "index_update_duration",
                "Index update duration (in seconds)",
                "step",
                metrics::default_duration_buckets(),
            ),
            update_size: metrics.histogram_vec(
                "index_update_size",
                "Index update size (in bytes)",
                "step",
                metrics::default_size_buckets(),
            ),
            height: metrics.gauge("index_height", "Indexed block height", "type"),
            db_properties: metrics.gauge("index_db_properties", "Index DB properties", "name"),
        }
    }

    fn observe_duration<T>(&self, label: &str, f: impl FnOnce() -> T) -> T {
        self.update_duration.observe_duration(label, f)
    }

    fn observe_size(&self, label: &str, rows: &[Row]) {
        self.update_size.observe(label, db_rows_size(rows) as f64);
    }

    fn observe_batch(&self, batch: &WriteBatch) {
        self.observe_size("write_funding_rows", &batch.funding_rows);
        self.observe_size("write_spending_rows", &batch.spending_rows);
        self.observe_size("write_txid_rows", &batch.txid_rows);
        self.observe_size("write_header_rows", &batch.header_rows);
        debug!(
            "writing {} funding and {} spending rows from {} transactions, {} blocks",
            batch.funding_rows.len(),
            batch.spending_rows.len(),
            batch.txid_rows.len(),
            batch.header_rows.len()
        );
    }

    fn observe_chain(&self, chain: &Chain) {
        self.height.set("tip", chain.height() as f64);
    }

    fn observe_db(&self, store: &DBStore) {
        for (cf, name, value) in store.get_properties() {
            self.db_properties
                .set(&format!("{}:{}", name, cf), value as f64);
        }
    }
}

struct IndexResult {
    header_row: HeaderRow,
    funding_rows: Vec<HashPrefixRow>,
    spending_rows: Vec<HashPrefixRow>,
    txid_rows: Vec<HashPrefixRow>,
}

impl IndexResult {
    fn extend(&self, batch: &mut WriteBatch) {
        let funding_rows = self.funding_rows.iter().map(HashPrefixRow::to_db_row);
        batch.funding_rows.extend(funding_rows);

        let spending_rows = self.spending_rows.iter().map(HashPrefixRow::to_db_row);
        batch.spending_rows.extend(spending_rows);

        let txid_rows = self.txid_rows.iter().map(HashPrefixRow::to_db_row);
        batch.txid_rows.extend(txid_rows);

        batch.header_rows.push(self.header_row.to_db_row());
        batch.tip_row = serialize(&self.header_row.header.block_hash()).into_boxed_slice();
    }
}

/// Confirmed transactions' address index
pub struct Index {
    store: DBStore,
    batch_size: usize,
    lookup_limit: Option<usize>,
    chain: Chain,
    stats: Stats,
    is_ready: bool,
}

impl Index {
    pub(crate) fn load(
        store: DBStore,
        mut chain: Chain,
        metrics: &Metrics,
        batch_size: usize,
        lookup_limit: Option<usize>,
        reindex_last_blocks: usize,
    ) -> Result<Self> {
        if let Some(row) = store.get_tip() {
            let tip = deserialize(&row).expect("invalid tip");
            let rows = store.read_headers();
            chain.load(
                rows.iter().map(|r| HeaderRow::from_db_row(r)).collect(),
                tip,
            );
            chain.drop_last_headers(reindex_last_blocks);
        };
        let stats = Stats::new(metrics);
        stats.observe_chain(&chain);
        stats.observe_db(&store);
        Ok(Index {
            store,
            batch_size,
            lookup_limit,
            chain,
            stats,
            is_ready: false,
        })
    }

    pub(crate) fn chain(&self) -> &Chain {
        &self.chain
    }

    pub(crate) fn limit_result<T>(&self, entries: impl Iterator<Item = T>) -> Result<Vec<T>> {
        let mut entries = entries.fuse();
        let result: Vec<T> = match self.lookup_limit {
            Some(lookup_limit) => entries.by_ref().take(lookup_limit).collect(),
            None => entries.by_ref().collect(),
        };
        if entries.next().is_some() {
            bail!(">{} index entries, query may take too long", result.len())
        }
        Ok(result)
    }

    // Note: the `filter_by_*` methods may return transactions from stale blocks..

    pub(crate) fn filter_by_txid(&self, txid: Txid) -> impl Iterator<Item = FilePosition> + '_ {
        self.store
            .iter_txid(TxidRow::scan_prefix(txid))
            .map(position_from_row)
    }

    pub(crate) fn filter_by_funding(
        &self,
        scripthash: ScriptHash,
    ) -> impl Iterator<Item = FilePosition> + '_ {
        self.store
            .iter_funding(ScriptHashRow::scan_prefix(scripthash))
            .map(position_from_row)
    }

    pub(crate) fn filter_by_spending(
        &self,
        outpoint: OutPoint,
    ) -> impl Iterator<Item = FilePosition> + '_ {
        self.store
            .iter_spending(SpendingPrefixRow::scan_prefix(outpoint))
            .map(position_from_row)
    }

    // Return `Ok(true)` when the chain is fully synced and the index is compacted.
    pub(crate) fn sync(&mut self, daemon: &Daemon, exit_flag: &ExitFlag) -> Result<bool> {
        let new_headers = self
            .stats
            .observe_duration("headers", || daemon.get_new_headers(&self.chain))?;
        if new_headers.is_empty() {
            // no more new headers
            self.store.flush(); // full compaction is performed on the first flush call
            self.is_ready = true; // the index is ready for queries
            return Ok(true); // no more blocks to index (sync is over)
        }
        let count = new_headers.len();
        info!("indexing {} blocks", count);
        let mut header_rows = Vec::with_capacity(new_headers.len());
        for chunk in new_headers.chunks(self.batch_size) {
            exit_flag.poll().with_context(|| {
                format!(
                    "indexing interrupted at block: {}",
                    chunk.first().unwrap().hash
                )
            })?;
            header_rows.extend(self.sync_blocks(daemon, chunk)?);
        }
        self.chain.update(header_rows);
        self.stats.height.set("tip", self.chain.height() as f64);
        self.stats.observe_chain(&self.chain);
        daemon.verify_blocks(&[self.chain.tip()])?; // sanity check
        Ok(false) // sync is not done
    }

    fn sync_blocks(
        &mut self,
        daemon: &Daemon,
        chunk: &[BlockHashPosition],
    ) -> Result<Vec<HeaderRow>> {
        let mut batch = WriteBatch::default();
        let mut header_rows = Vec::with_capacity(chunk.len());
        for h in chunk {
            self.stats.observe_duration("block", || -> Result<()> {
                let file = daemon.open_file(h.pos)?;
                let result = index_single_block(h.pos, file)?;
                result.extend(&mut batch); // FIXME
                header_rows.push(result.header_row);
                Ok(())
            })?;
        }
        batch.sort();
        self.stats.observe_batch(&batch);
        self.stats
            .observe_duration("write", || self.store.write(&batch));
        self.stats.observe_db(&self.store);
        Ok(header_rows)
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.is_ready
    }
}

fn position_from_row(row: Row) -> FilePosition {
    HashPrefixRow::from_db_row(&row).pos()
}

fn db_rows_size(rows: &[Row]) -> usize {
    rows.iter().map(|key| key.len()).sum()
}

fn index_single_block(block_pos: FilePosition, mut file: impl Read + Seek) -> Result<IndexResult> {
    let block_header = BlockHeader::consensus_decode(&mut file)?;
    let tx_count = VarInt::consensus_decode(&mut file)?.0 as usize;

    let mut funding_rows = Vec::with_capacity(tx_count);
    let mut spending_rows = Vec::with_capacity(tx_count);
    let mut txid_rows = Vec::with_capacity(tx_count);

    for _ in 0..tx_count {
        let offset = file.seek(SeekFrom::Current(0))?;
        let tx_pos = block_pos.with_offset(u32::try_from(offset)?);
        let tx = Transaction::consensus_decode(&mut file)?;
        txid_rows.push(TxidRow::row(tx.txid(), tx_pos));

        funding_rows.extend(
            tx.output
                .iter()
                .filter(|txo| !txo.script_pubkey.is_provably_unspendable())
                .map(|txo| {
                    let scripthash = ScriptHash::new(&txo.script_pubkey);
                    ScriptHashRow::row(scripthash, tx_pos)
                }),
        );

        if tx.is_coin_base() {
            continue; // coinbase doesn't have inputs
        }
        spending_rows.extend(
            tx.input
                .iter()
                .map(|txin| SpendingPrefixRow::row(txin.previous_output, tx_pos)),
        );
    }
    let block_limit = file.seek(SeekFrom::Current(0))?;
    let block_size = u32::try_from(block_limit)? - block_pos.offset;
    Ok(IndexResult {
        funding_rows,
        spending_rows,
        txid_rows,
        header_row: HeaderRow::new(block_header, block_pos, block_size),
    })
}
