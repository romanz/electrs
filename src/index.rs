use anyhow::Result;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::{Block, BlockHash, OutPoint, Txid};

use std::collections::HashMap;

use crate::{
    chain::Chain,
    daemon::Daemon,
    db,
    metrics::{Histogram, Metrics},
    types::{HeaderRow, ScriptHash, ScriptHashRow, SpendingPrefixRow, TxidRow},
};

#[derive(Clone)]
struct Stats {
    update_duration: Histogram,
    update_size: Histogram,
    lookup_duration: Histogram,
}

impl Stats {
    fn new(metrics: &Metrics) -> Self {
        Self {
            update_duration: metrics.histogram_vec(
                "index_update_duration",
                "Index update duration (in seconds)",
                &["step"],
            ),
            update_size: metrics.histogram_vec(
                "index_update_size",
                "Index update size (in bytes)",
                &["step"],
            ),
            lookup_duration: metrics.histogram_vec(
                "index_lookup_duration",
                "Index lookup duration (in seconds)",
                &["step"],
            ),
        }
    }

    fn report_stats(&self, batch: &db::WriteBatch) {
        self.update_size
            .observe_size("write_funding_rows", db_rows_size(&batch.funding_rows));
        self.update_size
            .observe_size("write_spending_rows", db_rows_size(&batch.spending_rows));
        self.update_size
            .observe_size("write_txid_rows", db_rows_size(&batch.txid_rows));
        self.update_size
            .observe_size("write_header_rows", db_rows_size(&batch.header_rows));
        debug!(
            "writing {} funding and {} spending rows from {} transactions, {} blocks",
            batch.funding_rows.len(),
            batch.spending_rows.len(),
            batch.txid_rows.len(),
            batch.header_rows.len()
        );
    }
}

struct IndexResult {
    header_row: HeaderRow,
    funding_rows: Vec<ScriptHashRow>,
    spending_rows: Vec<SpendingPrefixRow>,
    txid_rows: Vec<TxidRow>,
}

impl IndexResult {
    fn extend(&self, batch: &mut db::WriteBatch) {
        let funding_rows = self.funding_rows.iter().map(ScriptHashRow::to_db_row);
        let spending_rows = self.spending_rows.iter().map(SpendingPrefixRow::to_db_row);
        let txid_rows = self.txid_rows.iter().map(TxidRow::to_db_row);

        batch.funding_rows.extend(funding_rows);
        batch.spending_rows.extend(spending_rows);
        batch.txid_rows.extend(txid_rows);
        batch.header_rows.push(self.header_row.to_db_row());
        batch.tip_row = serialize(&self.header_row.header.block_hash()).into_boxed_slice();
    }
}

/// Confirmed transactions' address index
pub struct Index {
    store: db::DBStore,
    chain: Chain,
    stats: Stats,
}

impl Index {
    pub(crate) fn load(store: db::DBStore, mut chain: Chain, metrics: &Metrics) -> Result<Self> {
        if let Some(row) = store.get_tip() {
            let tip = deserialize(&row).expect("invalid tip");
            let headers = store
                .read_headers()
                .into_iter()
                .map(|row| HeaderRow::from_db_row(&row).header)
                .collect();
            chain.load(headers, tip);
        };

        Ok(Index {
            store,
            chain,
            stats: Stats::new(metrics),
        })
    }

    pub(crate) fn chain(&self) -> &Chain {
        &self.chain
    }

    pub(crate) fn filter_by_txid<'a>(&'a self, txid: Txid) -> impl Iterator<Item = BlockHash> + 'a {
        self.store
            .iter_txid(TxidRow::scan_prefix(txid))
            .map(|row| TxidRow::from_db_row(&row).height())
            .filter_map(move |height| self.chain.get_block_hash(height))
    }

    pub(crate) fn filter_by_funding<'a>(
        &'a self,
        scripthash: ScriptHash,
    ) -> impl Iterator<Item = BlockHash> + 'a {
        self.store
            .iter_funding(ScriptHashRow::scan_prefix(scripthash))
            .map(|row| ScriptHashRow::from_db_row(&row).height())
            .filter_map(move |height| self.chain.get_block_hash(height))
    }

    pub(crate) fn filter_by_spending<'a>(
        &'a self,
        outpoint: OutPoint,
    ) -> impl Iterator<Item = BlockHash> + 'a {
        self.store
            .iter_spending(SpendingPrefixRow::scan_prefix(outpoint))
            .map(|row| SpendingPrefixRow::from_db_row(&row).height())
            .filter_map(move |height| self.chain.get_block_hash(height))
    }

    pub(crate) fn sync(&mut self, daemon: &Daemon, chunk_size: usize) -> Result<()> {
        loop {
            let new_headers = daemon.get_new_headers(&self.chain)?;
            if new_headers.is_empty() {
                break;
            }
            info!(
                "indexing {} blocks: [{}..{}]",
                new_headers.len(),
                new_headers.first().unwrap().height(),
                new_headers.last().unwrap().height()
            );
            for chunk in new_headers.chunks(chunk_size) {
                let blockhashes: Vec<BlockHash> = chunk.iter().map(|h| h.hash()).collect();
                let mut heights_map: HashMap<BlockHash, usize> =
                    chunk.iter().map(|h| (h.hash(), h.height())).collect();

                let mut batch = db::WriteBatch::default();

                daemon.for_blocks(blockhashes, |blockhash, block| {
                    let height = heights_map.remove(&blockhash).expect("unexpected block");
                    let result = index_single_block(block, height);
                    result.extend(&mut batch);
                })?;
                assert!(heights_map.is_empty(), "some blocks were not indexed");
                batch.sort();
                self.stats.report_stats(&batch);
                self.store.write(batch);
            }
            self.chain.update(new_headers);
        }
        self.store.flush();
        Ok(())
    }
}

fn db_rows_size(rows: &[db::Row]) -> usize {
    rows.iter().map(|key| key.len()).sum()
}

fn index_single_block(block: Block, height: usize) -> IndexResult {
    let mut funding_rows = vec![];
    let mut spending_rows = vec![];
    let mut txid_rows = Vec::with_capacity(block.txdata.len());

    for tx in &block.txdata {
        txid_rows.push(TxidRow::new(tx.txid(), height));

        funding_rows.extend(
            tx.output
                .iter()
                .filter(|txo| !txo.script_pubkey.is_provably_unspendable())
                .map(|txo| {
                    let scripthash = ScriptHash::new(&txo.script_pubkey);
                    ScriptHashRow::new(scripthash, height)
                }),
        );

        if tx.is_coin_base() {
            continue; // coinbase doesn't have inputs
        }
        spending_rows.extend(
            tx.input
                .iter()
                .map(|txin| SpendingPrefixRow::new(txin.previous_output, height)),
        );
    }
    IndexResult {
        funding_rows,
        spending_rows,
        txid_rows,
        header_row: HeaderRow::new(block.header),
    }
}
