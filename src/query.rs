use bitcoin::blockdata::block::Block;
use bitcoin::blockdata::transaction::Transaction;
use bitcoin::network::serialize::deserialize;
use bitcoin::util::hash::Sha256dHash;
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use app::App;
use index::{compute_script_hash, TxInRow, TxOutRow, TxRow};
use mempool::Tracker;
use store::ReadStore;
use util::{FullHash, HashPrefix, HeaderEntry};

use errors::*;

struct FundingOutput {
    txn_id: Sha256dHash,
    height: u32,
    output_index: usize,
    value: u64,
}

struct SpendingInput {
    txn_id: Sha256dHash,
    height: u32,
    value: u64,
}

pub struct Status {
    confirmed: (Vec<FundingOutput>, Vec<SpendingInput>),
    mempool: (Vec<FundingOutput>, Vec<SpendingInput>),
}

fn calc_balance((funding, spending): &(Vec<FundingOutput>, Vec<SpendingInput>)) -> u64 {
    let funded_values = funding.iter().map(|output| output.value);
    let spent_values = spending.iter().map(|input| input.value);
    funded_values.sum::<u64>() - spent_values.sum::<u64>()
}

impl Status {
    fn funding(&self) -> impl Iterator<Item = &FundingOutput> {
        self.confirmed.0.iter().chain(self.mempool.0.iter())
    }

    fn spending(&self) -> impl Iterator<Item = &SpendingInput> {
        self.confirmed.1.iter().chain(self.mempool.1.iter())
    }

    pub fn confirmed_balance(&self) -> u64 {
        calc_balance(&self.confirmed)
    }

    pub fn mempool_balance(&self) -> u64 {
        calc_balance(&self.mempool)
    }

    pub fn history(&self) -> Vec<(i32, Sha256dHash)> {
        let mut txns_map = HashMap::<Sha256dHash, i32>::new();
        for f in self.funding() {
            txns_map.insert(f.txn_id, f.height as i32);
        }
        for s in self.spending() {
            txns_map.insert(s.txn_id, s.height as i32);
        }
        let mut txns: Vec<(i32, Sha256dHash)> =
            txns_map.into_iter().map(|item| (item.1, item.0)).collect();
        txns.sort();
        txns
    }

    pub fn hash(&self) -> Option<FullHash> {
        let txns = self.history();
        if txns.is_empty() {
            None
        } else {
            let mut hash = FullHash::default();
            let mut sha2 = Sha256::new();
            for (height, txn_id) in txns {
                let part = format!("{}:{}:", txn_id.be_hex_string(), height);
                sha2.input(part.as_bytes());
            }
            sha2.result(&mut hash);
            Some(hash)
        }
    }
}

struct TxnHeight {
    txn: Transaction,
    height: u32,
}

fn merklize(left: Sha256dHash, right: Sha256dHash) -> Sha256dHash {
    let data = [&left[..], &right[..]].concat();
    Sha256dHash::from_data(&data)
}

// TODO: the 3 functions below can be part of ReadStore.
fn txrows_by_prefix(store: &ReadStore, txid_prefix: &HashPrefix) -> Vec<TxRow> {
    store
        .scan(&TxRow::filter(&txid_prefix))
        .iter()
        .map(|row| TxRow::from_row(row))
        .collect()
}

fn txids_by_script_hash(store: &ReadStore, script_hash: &[u8]) -> Vec<HashPrefix> {
    store
        .scan(&TxOutRow::filter(script_hash))
        .iter()
        .map(|row| TxOutRow::from_row(row).txid_prefix)
        .collect()
}

fn txids_by_funding_output(
    store: &ReadStore,
    txn_id: &Sha256dHash,
    output_index: usize,
) -> Vec<HashPrefix> {
    store
        .scan(&TxInRow::filter(&txn_id, output_index))
        .iter()
        .map(|row| TxInRow::from_row(row).txid_prefix)
        .collect()
}

pub struct Query {
    app: Arc<App>,
    tracker: RwLock<Tracker>,
}

impl Query {
    pub fn new(app: Arc<App>) -> Query {
        Query {
            app,
            tracker: RwLock::new(Tracker::new()),
        }
    }

    fn load_txns(&self, store: &ReadStore, prefixes: Vec<HashPrefix>) -> Result<Vec<TxnHeight>> {
        let mut txns = vec![];
        for txid_prefix in prefixes {
            for tx_row in txrows_by_prefix(store, &txid_prefix) {
                let txid: Sha256dHash = deserialize(&tx_row.key.txid).unwrap();
                let txn: Transaction = self.get_tx(&txid)?;
                txns.push(TxnHeight {
                    txn,
                    height: tx_row.height,
                })
            }
        }
        Ok(txns)
    }

    fn find_spending_input(
        &self,
        store: &ReadStore,
        funding: &FundingOutput,
    ) -> Result<Option<SpendingInput>> {
        let spending_txns: Vec<TxnHeight> = self.load_txns(
            store,
            txids_by_funding_output(store, &funding.txn_id, funding.output_index),
        )?;
        let mut spending_inputs = vec![];
        for t in &spending_txns {
            for input in t.txn.input.iter() {
                if input.prev_hash == funding.txn_id
                    && input.prev_index == funding.output_index as u32
                {
                    spending_inputs.push(SpendingInput {
                        txn_id: t.txn.txid(),
                        height: t.height,
                        value: funding.value,
                    })
                }
            }
        }
        assert!(spending_inputs.len() <= 1);
        Ok(if spending_inputs.len() == 1 {
            Some(spending_inputs.remove(0))
        } else {
            None
        })
    }

    fn find_funding_outputs(&self, t: &TxnHeight, script_hash: &[u8]) -> Vec<FundingOutput> {
        let mut result = vec![];
        let txn_id = t.txn.txid();
        for (index, output) in t.txn.output.iter().enumerate() {
            if compute_script_hash(&output.script_pubkey[..]) == script_hash {
                result.push(FundingOutput {
                    txn_id: txn_id,
                    height: t.height,
                    output_index: index,
                    value: output.value,
                })
            }
        }
        result
    }

    fn confirmed_status(
        &self,
        script_hash: &[u8],
    ) -> Result<(Vec<FundingOutput>, Vec<SpendingInput>)> {
        let mut funding = vec![];
        let mut spending = vec![];
        for t in self.load_txns(
            self.app.store(),
            txids_by_script_hash(self.app.store(), script_hash),
        )? {
            funding.extend(self.find_funding_outputs(&t, script_hash));
        }
        for funding_output in &funding {
            if let Some(spent) = self.find_spending_input(self.app.store(), &funding_output)? {
                spending.push(spent);
            }
        }
        Ok((funding, spending))
    }

    fn mempool_status(
        &self,
        script_hash: &[u8],
        confirmed_funding: &[FundingOutput],
    ) -> Result<(Vec<FundingOutput>, Vec<SpendingInput>)> {
        let mut funding = vec![];
        let mut spending = vec![];
        let tracker = self.tracker.read().unwrap();
        for t in self.load_txns(
            tracker.index(),
            txids_by_script_hash(tracker.index(), script_hash),
        )? {
            funding.extend(self.find_funding_outputs(&t, script_hash));
        }
        // // TODO: dedup outputs (somehow) both confirmed and in mempool (e.g. reorg?)
        for funding_output in funding.iter().chain(confirmed_funding.iter()) {
            if let Some(spent) = self.find_spending_input(tracker.index(), &funding_output)? {
                spending.push(spent);
            }
        }
        Ok((funding, spending))
    }

    pub fn status(&self, script_hash: &[u8]) -> Result<Status> {
        let confirmed = self.confirmed_status(script_hash)?;
        let mempool = self.mempool_status(script_hash, &confirmed.0)?;
        Ok(Status { confirmed, mempool })
    }

    pub fn get_tx(&self, tx_hash: &Sha256dHash) -> Result<Transaction> {
        self.app.daemon().gettransaction(tx_hash)
    }

    pub fn get_headers(&self, heights: &[usize]) -> Vec<HeaderEntry> {
        let index = self.app.index();
        heights
            .iter()
            .filter_map(|height| index.get_header(*height))
            .collect()
    }

    pub fn get_best_header(&self) -> Result<HeaderEntry> {
        let last_header = self.app.index().best_header();
        Ok(last_header.chain_err(|| "no headers indexed")?.clone())
    }

    pub fn get_merkle_proof(
        &self,
        tx_hash: &Sha256dHash,
        height: usize,
    ) -> Result<(Vec<Sha256dHash>, usize)> {
        let header_entry = self.app
            .index()
            .get_header(height)
            .chain_err(|| format!("missing block #{}", height))?;
        let block: Block = self.app.daemon().getblock(&header_entry.hash())?;
        let mut txids: Vec<Sha256dHash> = block.txdata.iter().map(|tx| tx.txid()).collect();
        let pos = txids
            .iter()
            .position(|txid| txid == tx_hash)
            .chain_err(|| format!("missing txid {}", tx_hash))?;
        let mut merkle = vec![];
        let mut index = pos;
        while txids.len() > 1 {
            if txids.len() % 2 != 0 {
                let last = txids.last().unwrap().clone();
                txids.push(last);
            }
            index = if index % 2 == 0 { index + 1 } else { index - 1 };
            merkle.push(txids[index]);
            index = index / 2;
            txids = txids
                .chunks(2)
                .map(|pair| merklize(pair[0], pair[1]))
                .collect()
        }
        Ok((merkle, pos))
    }

    pub fn broadcast(&self, txn: &Transaction) -> Result<Sha256dHash> {
        self.app.daemon().broadcast(txn)
    }

    pub fn update_mempool(&self) -> Result<()> {
        self.tracker.write().unwrap().update(self.app.daemon())
    }

    /// Returns [vsize, fee_rate] pairs (measured in vbytes and satoshis).
    pub fn get_fee_histogram(&self) -> Vec<(f32, u32)> {
        self.tracker.read().unwrap().fee_histogram().clone()
    }

    // Fee rate [BTC/kB] to be confirmed in `blocks` from now.
    pub fn estimate_fee(&self, blocks: usize) -> f32 {
        let mut total_vsize = 0u32;
        let mut last_fee_rate = 0.0;
        let blocks_in_vbytes = (blocks * 1_000_000) as u32; // assume ~1MB blocks
        for (fee_rate, vsize) in self.tracker.read().unwrap().fee_histogram() {
            last_fee_rate = *fee_rate;
            total_vsize += vsize;
            if total_vsize >= blocks_in_vbytes {
                break; // under-estimate the fee rate a bit
            }
        }
        last_fee_rate * 1e-5 // [BTC/kB] = 10^5 [sat/B]
    }
}
