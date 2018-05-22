use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::blockdata::transaction::Transaction;
use bitcoin::network::serialize::deserialize;
use bitcoin::util::hash::Sha256dHash;
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use std::collections::HashMap;
use std::sync::RwLock;

use daemon::Daemon;
use index::{compute_script_hash, Index, TxInRow, TxOutRow, TxRow};
use mempool::Tracker;
use store::ReadStore;
use util::{FullHash, HashPrefix, HeaderEntry};

error_chain!{}

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
    funding: Vec<FundingOutput>,
    spending: Vec<SpendingInput>,
}

impl Status {
    pub fn balance(&self) -> u64 {
        let total = self.funding
            .iter()
            .fold(0, |acc, output| acc + output.value);
        self.spending
            .iter()
            .fold(total, |acc, input| acc - input.value)
    }

    pub fn history(&self) -> Vec<(i32, Sha256dHash)> {
        let mut txns_map = HashMap::<Sha256dHash, i32>::new();
        for f in &self.funding {
            txns_map.insert(f.txn_id, f.height as i32);
        }
        for s in &self.spending {
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

pub struct Query<'a> {
    daemon: &'a Daemon,
    index: &'a Index,
    index_store: &'a ReadStore, // TODO: should be a part of index
    tracker: RwLock<Tracker>,
}

// TODO: return errors instead of panics
impl<'a> Query<'a> {
    pub fn new(index_store: &'a ReadStore, daemon: &'a Daemon, index: &'a Index) -> Query<'a> {
        Query {
            daemon,
            index,
            index_store,
            tracker: RwLock::new(Tracker::new()),
        }
    }

    pub fn daemon(&self) -> &Daemon {
        self.daemon
    }

    fn load_txns(&self, store: &ReadStore, prefixes: Vec<HashPrefix>) -> Vec<TxnHeight> {
        let mut txns = Vec::new();
        for txid_prefix in prefixes {
            for tx_row in txrows_by_prefix(store, &txid_prefix) {
                let txid: Sha256dHash = deserialize(&tx_row.key.txid).unwrap();
                let txn: Transaction = self.get_tx(&txid);
                txns.push(TxnHeight {
                    txn,
                    height: tx_row.height,
                })
            }
        }
        txns
    }

    fn find_spending_input(
        &self,
        store: &ReadStore,
        funding: &FundingOutput,
    ) -> Option<SpendingInput> {
        let spending_txns: Vec<TxnHeight> = self.load_txns(
            store,
            txids_by_funding_output(store, &funding.txn_id, funding.output_index),
        );
        let mut spending_inputs = Vec::new();
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
        if spending_inputs.len() == 1 {
            Some(spending_inputs.remove(0))
        } else {
            None
        }
    }

    fn find_funding_outputs(&self, t: &TxnHeight, script_hash: &[u8]) -> Vec<FundingOutput> {
        let mut result = Vec::new();
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

    fn confirmed_status(&self, script_hash: &[u8]) -> Status {
        let mut funding = vec![];
        let mut spending = vec![];
        for t in self.load_txns(
            self.index_store,
            txids_by_script_hash(self.index_store, script_hash),
        ) {
            funding.extend(self.find_funding_outputs(&t, script_hash));
        }
        for funding_output in &funding {
            if let Some(spent) = self.find_spending_input(self.index_store, &funding_output) {
                spending.push(spent);
            }
        }
        Status { funding, spending }
    }

    fn mempool_status(&self, script_hash: &[u8], confirmed_status: &Status) -> Status {
        let mut funding = vec![];
        let mut spending = vec![];
        let tracker = self.tracker.read().unwrap();
        for t in self.load_txns(
            tracker.index(),
            txids_by_script_hash(tracker.index(), script_hash),
        ) {
            funding.extend(self.find_funding_outputs(&t, script_hash));
        }
        // // TODO: dedup outputs (somehow) both confirmed and in mempool (e.g. reorg?)
        for funding_output in funding.iter().chain(confirmed_status.funding.iter()) {
            if let Some(spent) = self.find_spending_input(tracker.index(), &funding_output) {
                spending.push(spent);
            }
        }
        // TODO: update height to -1 for txns with any unconfirmed input
        // (https://electrumx.readthedocs.io/en/latest/protocol-basics.html#status)
        Status { funding, spending }
    }

    pub fn status(&self, script_hash: &[u8]) -> Status {
        let mut status = self.confirmed_status(script_hash);
        let mempool_status = self.mempool_status(script_hash, &status);
        status.funding.extend(mempool_status.funding);
        status.spending.extend(mempool_status.spending);
        status
    }

    pub fn get_tx(&self, tx_hash: &Sha256dHash) -> Transaction {
        self.daemon
            .gettransaction(tx_hash)
            .expect(&format!("failed to load tx {}", tx_hash))
    }

    pub fn get_headers(&self, heights: &[usize]) -> Vec<BlockHeader> {
        let headers_list = self.index.headers_list();
        let headers = headers_list.headers();
        let mut result = Vec::new();
        for height in heights {
            let header: &BlockHeader = match headers.get(*height) {
                Some(header) => header.header(),
                None => break,
            };
            result.push(*header);
        }
        result
    }

    pub fn get_best_header(&self) -> Option<HeaderEntry> {
        let header_list = self.index.headers_list();
        Some(header_list.headers().last()?.clone())
    }

    // TODO: add error-handling logic
    pub fn get_merkle_proof(
        &self,
        tx_hash: &Sha256dHash,
        height: usize,
    ) -> Option<(Vec<Sha256dHash>, usize)> {
        let header_list = self.index.headers_list();
        let blockhash = header_list.headers().get(height)?.hash();
        let block: Block = self.daemon.getblock(&blockhash).unwrap();
        let mut txids: Vec<Sha256dHash> = block.txdata.iter().map(|tx| tx.txid()).collect();
        let pos = txids.iter().position(|txid| txid == tx_hash)?;
        let mut merkle = Vec::new();
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
        Some((merkle, pos))
    }

    pub fn update_mempool(&self) -> Result<()> {
        self.tracker
            .write()
            .unwrap()
            .update(self.daemon)
            .chain_err(|| "failed to update mempool")
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
