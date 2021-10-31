use anyhow::{Context, Result};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::{Block, BlockHash, OutPoint, Txid};

use crate::{
    chain::Chain,
    daemon::Daemon,
    db::{DBStore, Row, WriteBatch},
    metrics::{self, Gauge, Histogram, Metrics},
    signals::ExitFlag,
    types::{HashPrefixRow, HeaderRow, ScriptHash, ScriptHashRow, SpendingPrefixRow, TxidRow},
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
        self.update_size.observe(label, db_rows_size(rows));
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
            let headers = store
                .read_headers()
                .into_iter()
                .map(|row| HeaderRow::from_db_row(&row).header)
                .collect();
            chain.load(headers, tip);
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

    pub(crate) fn filter_by_txid(&self, txid: Txid) -> impl Iterator<Item = BlockHash> + '_ {
        self.store
            .iter_txid(TxidRow::scan_prefix(txid))
            .map(|row| HashPrefixRow::from_db_row(&row).height())
            .filter_map(move |height| self.chain.get_block_hash(height))
    }

    pub(crate) fn filter_by_funding(
        &self,
        scripthash: ScriptHash,
    ) -> impl Iterator<Item = BlockHash> + '_ {
        self.store
            .iter_funding(ScriptHashRow::scan_prefix(scripthash))
            .map(|row| HashPrefixRow::from_db_row(&row).height())
            .filter_map(move |height| self.chain.get_block_hash(height))
    }

    pub(crate) fn filter_by_spending(
        &self,
        outpoint: OutPoint,
    ) -> impl Iterator<Item = BlockHash> + '_ {
        self.store
            .iter_spending(SpendingPrefixRow::scan_prefix(outpoint))
            .map(|row| HashPrefixRow::from_db_row(&row).height())
            .filter_map(move |height| self.chain.get_block_hash(height))
    }

    pub(crate) fn sync(&mut self, daemon: &Daemon, exit_flag: &ExitFlag) -> Result<()> {
        self.stats.observe_db(&self.store);
        loop {
            let new_headers = self
                .stats
                .observe_duration("headers", || daemon.get_new_headers(&self.chain))?;
            if new_headers.is_empty() {
                break;
            }
            info!(
                "indexing {} blocks: [{}..{}]",
                new_headers.len(),
                new_headers.first().unwrap().height(),
                new_headers.last().unwrap().height()
            );
            for chunk in new_headers.chunks(self.batch_size) {
                exit_flag.poll().with_context(|| {
                    format!(
                        "indexing interrupted at height: {}",
                        chunk.first().unwrap().height()
                    )
                })?;
                let blockhashes: Vec<BlockHash> = chunk.iter().map(|h| h.hash()).collect();
                let mut heights = chunk.iter().map(|h| h.height());

                let mut batch = WriteBatch::default();
                daemon.for_blocks(blockhashes, |_blockhash, block| {
                    let height = heights.next().expect("unexpected block");
                    self.stats.observe_duration("block", || {
                        index_single_block(block, height).extend(&mut batch)
                    });
                    self.stats.height.set("tip", height as f64);
                })?;
                let heights: Vec<_> = heights.collect();
                assert!(
                    heights.is_empty(),
                    "some blocks were not indexed: {:?}",
                    heights
                );
                batch.sort();
                self.stats.observe_batch(&batch);
                self.stats
                    .observe_duration("write", || self.store.write(&batch));
                self.stats.observe_db(&self.store);
            }
            self.chain.update(new_headers);
            self.stats.observe_chain(&self.chain);
        }
        self.store.flush();
        Ok(())
    }
}

fn db_rows_size(rows: &[Row]) -> usize {
    rows.iter().map(|key| key.len()).sum()
}

fn index_single_block(block: Block, height: usize) -> IndexResult {
    let mut funding_rows = Vec::with_capacity(block.txdata.iter().map(|tx| tx.output.len()).sum());
    let mut spending_rows = Vec::with_capacity(block.txdata.iter().map(|tx| tx.input.len()).sum());
    let mut txid_rows = Vec::with_capacity(block.txdata.len());

    for tx in &block.txdata {
        txid_rows.push(TxidRow::row(tx.txid(), height));

        funding_rows.extend(
            tx.output
                .iter()
                .filter(|txo| !txo.script_pubkey.is_provably_unspendable())
                .map(|txo| {
                    let scripthash = ScriptHash::new(&txo.script_pubkey);
                    ScriptHashRow::row(scripthash, height)
                }),
        );

        if tx.is_coin_base() {
            continue; // coinbase doesn't have inputs
        }
        spending_rows.extend(
            tx.input
                .iter()
                .map(|txin| SpendingPrefixRow::row(txin.previous_output, height)),
        );
    }
    IndexResult {
        funding_rows,
        spending_rows,
        txid_rows,
        header_row: HeaderRow::new(block.header),
    }
}
