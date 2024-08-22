use anyhow::{Context, Result};
use bitcoin::consensus::{deserialize, Decodable, Encodable};
use bitcoin::secp256k1::PublicKey;
use bitcoin::hashes::Hash;
use bitcoin::{BlockHash, OutPoint, Txid, XOnlyPublicKey};
use bitcoin_slices::{bsl, Visit, Visitor};
use std::collections::HashMap;
use std::ops::ControlFlow;

use crate::{
    chain::{Chain, NewHeader},
    daemon::Daemon,
    db::{DBStore, WriteBatch},
    metrics::{self, Gauge, Histogram, Metrics},
    signals::ExitFlag,
    types::{
        bsl_txid, HashPrefixRow, HeaderRow, ScriptHash, ScriptHashRow, SerBlock, SpendingPrefixRow,
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

    fn observe_size<const N: usize>(&self, label: &str, rows: &[[u8; N]]) {
        self.update_size.observe(label, (rows.len() * N) as f64);
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

/// Confirmed transactions' address index
pub struct Index {
    store: DBStore,
    batch_size: usize,
    lookup_limit: Option<usize>,
    chain: Chain,
    stats: Stats,
    is_ready: bool,
    is_sp_ready: bool,
    flush_needed: bool,
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
                .iter_headers()
                .map(|row| HeaderRow::from_db_row(row).header);
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
            is_ready: false,
            is_sp_ready: false,
            flush_needed: false,
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
            .map(|row| HashPrefixRow::from_db_row(row).height())
            .filter_map(move |height| self.chain.get_block_hash(height))
    }

    pub(crate) fn filter_by_funding(
        &self,
        scripthash: ScriptHash,
    ) -> impl Iterator<Item = BlockHash> + '_ {
        self.store
            .iter_funding(ScriptHashRow::scan_prefix(scripthash))
            .map(|row| HashPrefixRow::from_db_row(row).height())
            .filter_map(move |height| self.chain.get_block_hash(height))
    }

    pub(crate) fn filter_by_spending(
        &self,
        outpoint: OutPoint,
    ) -> impl Iterator<Item = BlockHash> + '_ {
        self.store
            .iter_spending(SpendingPrefixRow::scan_prefix(outpoint))
            .map(|row| HashPrefixRow::from_db_row(row).height())
            .filter_map(move |height| self.chain.get_block_hash(height))
    }
    pub(crate) fn silent_payments_sync(
        &mut self,
        daemon: &Daemon,
        exit_flag: &ExitFlag,
    ) -> Result<bool> {

        let mut new_headers: Vec<NewHeader> = Vec::with_capacity(2000);
        let start: usize;
        if let Some(row) = self.store.last_sp() {
            let blockhash: BlockHash = deserialize(&row).expect("invalid block_hash");
            start = self.chain.get_block_height(&blockhash).expect("Can't find block_hash") + 1;
        } else {
            start = 70_000;
        }
        let end = if start + 2000 < self.chain.height() {
            start + 2000
        } else {
            self.chain.height()
        };
        for block_height in start..end {
            new_headers.push(NewHeader::from((
                *self
                    .chain
                    .get_block_header(block_height)
                    .expect("Unexpected missing block header"),
                block_height,
            )));
        }
        match (new_headers.first(), new_headers.last()) {
            (Some(first), Some(last)) => {
                let count = new_headers.len();
                info!(
                    "Looking for sp tweaks in {} blocks: [{}..{}]",
                    count,
                    first.height(),
                    last.height()
                );
            }
            _ => {
                if self.flush_needed {
                    self.store.flush(); // full compaction is performed on the first flush call
                    self.flush_needed = false;
                }
                self.is_sp_ready = true;
                return Ok(true); // no more blocks to index (done for now)
            }
        }
        for chunk in new_headers.chunks(self.batch_size) {
            exit_flag.poll().with_context(|| {
                format!(
                    "indexing sp interrupted at height: {}",
                    chunk.first().unwrap().height()
                )
            })?;
            self.sync_blocks(daemon, chunk, true)?;
        }
        self.flush_needed = true;
        Ok(false) // sync is not done
    }

    // Return `Ok(true)` when the chain is fully synced and the index is compacted.
    pub(crate) fn sync(&mut self, daemon: &Daemon, exit_flag: &ExitFlag) -> Result<bool> {
        let new_headers = self
            .stats
            .observe_duration("headers", || daemon.get_new_headers(&self.chain))?;
        match (new_headers.first(), new_headers.last()) {
            (Some(first), Some(last)) => {
                let count = new_headers.len();
                info!(
                    "indexing {} blocks: [{}..{}]",
                    count,
                    first.height(),
                    last.height()
                );
            }
            _ => {
                if self.flush_needed {
                    self.store.flush(); // full compaction is performed on the first flush call
                    self.flush_needed = false;
                }
                self.is_ready = true;
                return Ok(true); // no more blocks to index (done for now)
            }
        }
        for chunk in new_headers.chunks(self.batch_size) {
            exit_flag.poll().with_context(|| {
                format!(
                    "indexing interrupted at height: {}",
                    chunk.first().unwrap().height()
                )
            })?;
            self.sync_blocks(daemon, chunk, false)?;
        }
        self.chain.update(new_headers);
        self.stats.observe_chain(&self.chain);
        self.flush_needed = true;
        Ok(false) // sync is not done
    }

    fn sync_blocks(&mut self, daemon: &Daemon, chunk: &[NewHeader], sp: bool) -> Result<()> {
        let blockhashes: Vec<BlockHash> = chunk.iter().map(|h| h.hash()).collect();
        let mut heights = chunk.iter().map(|h| h.height());

        let mut batch = WriteBatch::default();
        if !sp {
            let scan_block = |blockhash, block| {
                let height = heights.next().expect("unexpected block");
                self.stats.observe_duration("block", || {
                    index_single_block(blockhash, block, height, &mut batch);
                });
                self.stats.height.set("tip", height as f64);
            };

            daemon.for_blocks(blockhashes, scan_block)?;
        } else {
            let scan_block_for_sp = |blockhash, block| {
                let height = heights.next().expect("unexpected block");
                self.stats.observe_duration("block_sp", || {
                    scan_single_block_for_silent_payments(
                        self,
                        daemon,
                        blockhash,
                        block,
                        &mut batch,
                    );
                });
                self.stats.height.set("sp", height as f64);
            };

            daemon.for_blocks(blockhashes, scan_block_for_sp)?;
        }

        let heights: Vec<_> = heights.collect();
        assert!(
            heights.is_empty(),
            "some blocks were not indexed: {:?}",
            heights
        );
        batch.sort();
        self.stats.observe_batch(&batch);
        if !sp {
            self.stats
                .observe_duration("write", || self.store.write(&batch));
        } else {
            self.stats
                .observe_duration("write_sp", || self.store.write_sp(&batch));
        }
        
        self.stats.observe_db(&self.store);
        Ok(())
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.is_ready
    }

    pub(crate) fn is_sp_ready(&self) -> bool {
        self.is_sp_ready
    }
}

fn index_single_block(
    block_hash: BlockHash,
    block: SerBlock,
    height: usize,
    batch: &mut WriteBatch,
) {
    struct IndexBlockVisitor<'a> {
        batch: &'a mut WriteBatch,
        height: usize,
    }

    impl<'a> Visitor for IndexBlockVisitor<'a> {
        fn visit_transaction(&mut self, tx: &bsl::Transaction) -> ControlFlow<()> {
            let txid = bsl_txid(tx);
            self.batch
                .txid_rows
                .push(TxidRow::row(txid, self.height).to_db_row());
            ControlFlow::Continue(())
        }

        fn visit_tx_out(&mut self, _vout: usize, tx_out: &bsl::TxOut) -> ControlFlow<()> {
            let script = bitcoin::Script::from_bytes(tx_out.script_pubkey());
            // skip indexing unspendable outputs
            if !script.is_op_return() {
                let row = ScriptHashRow::row(ScriptHash::new(script), self.height);
                self.batch.funding_rows.push(row.to_db_row());
            }
            ControlFlow::Continue(())
        }

        fn visit_tx_in(&mut self, _vin: usize, tx_in: &bsl::TxIn) -> ControlFlow<()> {
            let prevout: OutPoint = tx_in.prevout().into();
            // skip indexing coinbase transactions' input
            if !prevout.is_null() {
                let row = SpendingPrefixRow::row(prevout, self.height);
                self.batch.spending_rows.push(row.to_db_row());
            }
            ControlFlow::Continue(())
        }

        fn visit_block_header(&mut self, header: &bsl::BlockHeader) -> ControlFlow<()> {
            let header = bitcoin::block::Header::consensus_decode(&mut header.as_ref())
                .expect("block header was already validated");
            self.batch
                .header_rows
                .push(HeaderRow::new(header).to_db_row());
            ControlFlow::Continue(())
        }
    }

    let mut index_block = IndexBlockVisitor { batch, height };
    bsl::Block::visit(&block, &mut index_block).expect("core returned invalid block");

    let len = block_hash
        .consensus_encode(&mut (&mut batch.tip_row as &mut [u8]))
        .expect("in-memory writers don't error");
    debug_assert_eq!(len, BlockHash::LEN);
}

fn scan_single_block_for_silent_payments(
    index: &Index,
    daemon: &Daemon,
    block_hash: BlockHash,
    block: SerBlock,
    batch: &mut WriteBatch,
) {
    struct IndexBlockVisitor<'a> {
        daemon: &'a Daemon,
        index: &'a Index,
        map: &'a mut HashMap<BlockHash, Vec<u8>>,
    }

    impl<'a> Visitor for IndexBlockVisitor<'a> {
        fn visit_transaction(&mut self, tx: &bsl::Transaction) -> core::ops::ControlFlow<()> {
            let parsed_tx: bitcoin::Transaction = match deserialize(tx.as_ref()) {
                Ok(tx) => tx,
                Err(_) => panic!("Unexpected invalid transaction"),
            };

            if parsed_tx.is_coinbase() { return ControlFlow::Continue(()) };

            let txid = bsl_txid(tx);

            let mut to_scan = false;
            for (i, o) in parsed_tx.output.iter().enumerate() {
                if o.script_pubkey.is_p2tr() {
                    let outpoint = OutPoint {
                        txid,
                        vout: i.try_into().expect("Unexpectedly high vout"),
                    };
                    if self
                        .index
                        .store
                        .iter_spending(SpendingPrefixRow::scan_prefix(outpoint))
                        .next()
                        .is_none()
                    {
                        to_scan = true;
                        break; // Stop iterating once a relevant P2TR output is found
                    }
                }
            }

            if !to_scan {
                return ControlFlow::Continue(());
            }

            // Iterate over inputs
            let mut pubkeys: Vec<PublicKey> = Vec::new();
            let mut xonly_pubkeys: Vec<XOnlyPublicKey> = Vec::new();
            let mut outpoints: Vec<(Txid, u32)> = Vec::with_capacity(parsed_tx.input.len());
            for i in parsed_tx.input.iter() {
                outpoints.push((i.previous_output.txid, i.previous_output.vout));
                let prev_tx: bitcoin::Transaction = self
                    .daemon
                    .get_transaction(&i.previous_output.txid, None)
                    .expect("Spending non existent UTXO");
                let index: usize = i
                    .previous_output
                    .vout
                    .try_into()
                    .expect("Unexpectedly high vout");
                let prevout: &bitcoin::TxOut = prev_tx
                    .output
                    .get(index)
                    .expect("Spending a non existent UTXO");
                match crate::sp::get_pubkey_from_input(&crate::sp::VinData {
                    script_sig: i.script_sig.to_bytes(),
                    txinwitness: i.witness.to_vec(),
                    script_pub_key: prevout.script_pubkey.to_bytes(),
                }) {
                    Ok(Some(pubkey_from_input)) => match pubkey_from_input {
                        crate::sp::PubKeyFromInput::XOnlyPublicKey(xonly_pubkey) => xonly_pubkeys.push(xonly_pubkey),
                        crate::sp::PubKeyFromInput::PublicKey(pubkey) => pubkeys.push(pubkey), 
                    },
                    Ok(None) => (),
                    Err(_) => panic!("Scanning for public keys failed for tx: {}", txid),
                }
            }
            let pubkeys_ref: Vec<&PublicKey> = pubkeys.iter().collect();
            let pubkeys_ref = pubkeys_ref.as_slice();

            // if the pubkeys have opposite parity, the combine_pubkey_result will be Err
            let combine_pubkey_result = PublicKey::combine_keys(pubkeys_ref);

            if combine_pubkey_result.is_ok() && (!pubkeys.is_empty() || !xonly_pubkeys.is_empty()) {

                let input_pub_keys = if pubkeys.is_empty() {
                    None
                } else {
                    Some(pubkeys.as_slice())
                }; 

                let input_xpub_keys = if xonly_pubkeys.is_empty() {
                    None
                } else {
                    Some(xonly_pubkeys.as_slice())
                }; 
                
                let tweak = crate::sp::recipient_calculate_tweak_data(input_xpub_keys, input_pub_keys, &outpoints).unwrap();

                if let Some(block_hash) = self.index.filter_by_txid(txid).next() {
                    if let Some(value) = self.map.get_mut(&block_hash) {
                        value.extend(tweak.iter());
                    } else {
                        self.map.insert(block_hash, Vec::from_iter(tweak)); 
                    }
                } else {
                    panic!("Unexpected unknown transaction");
                }
            }

            ControlFlow::Continue(())
        }
    }

    let mut map: HashMap<BlockHash, Vec<u8>> = HashMap::with_capacity(index.batch_size);
    let mut index_block = IndexBlockVisitor {
        daemon,
        index,
        map: &mut map
    };
    bsl::Block::visit(&block, &mut index_block).expect("core returned invalid block");
    for (hash, tweaks) in map {
        let height = index.chain.get_block_height(&hash).expect("Unexpected non existing blockhash");
        let mut value: Vec<u8> = u64::try_from(height).expect("Unexpected invalid usize").to_be_bytes().to_vec();
        value.extend(tweaks.iter());
        batch.tweak_rows.push(value);
    }
    let len = block_hash
        .consensus_encode(&mut (&mut batch.sp_tip_row as &mut [u8]))
        .expect("in-memory writers don't error");
    debug_assert_eq!(len, BlockHash::LEN);
}
