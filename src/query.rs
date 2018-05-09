use bincode;
use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::blockdata::transaction::Transaction;
use bitcoin::network::serialize::deserialize;
use bitcoin::util::hash::Sha256dHash;
use itertools::enumerate;

use daemon::Daemon;
use index::{compute_script_hash, hash_prefix, HashPrefix, HeaderEntry, Index, TxInKey, TxInRow,
            TxKey, TxOutRow, HASH_PREFIX_LEN};
use store::Store;

pub struct Query<'a> {
    store: &'a Store,
    daemon: &'a Daemon,
    index: &'a Index,
}

pub struct FundingOutput {
    pub txn_id: Sha256dHash,
    pub height: u32,
    pub output_index: usize,
    pub value: u64,
}

pub struct SpendingInput {
    pub txn_id: Sha256dHash,
    pub height: u32,
    pub input_index: usize,
}

pub struct Status {
    pub balance: u64,
    pub funding: Vec<FundingOutput>,
    pub spending: Vec<SpendingInput>,
}

struct TxnHeight {
    txn: Transaction,
    height: u32,
}

fn merklize(left: Sha256dHash, right: Sha256dHash) -> Sha256dHash {
    let data = [&left[..], &right[..]].concat();
    Sha256dHash::from_data(&data)
}

// TODO: return errors instead of panics
impl<'a> Query<'a> {
    pub fn new(store: &'a Store, daemon: &'a Daemon, index: &'a Index) -> Query<'a> {
        Query {
            store,
            daemon,
            index,
        }
    }

    fn load_txns(&self, prefixes: Vec<HashPrefix>) -> Vec<TxnHeight> {
        let mut txns = Vec::new();
        for txid_prefix in prefixes {
            for row in self.store.scan(&[b"T", &txid_prefix[..]].concat()) {
                let key: TxKey = bincode::deserialize(&row.key).unwrap();
                let txid: Sha256dHash = deserialize(&key.txid).unwrap();
                let txn: Transaction = self.get_tx(&txid);
                let height: u32 = bincode::deserialize(&row.value).unwrap();
                txns.push(TxnHeight { txn, height })
            }
        }
        txns
    }

    fn find_spending_input(&self, funding: &FundingOutput) -> Option<SpendingInput> {
        let spend_key = bincode::serialize(&TxInKey {
            code: b'I',
            prev_hash_prefix: hash_prefix(&funding.txn_id[..]),
            prev_index: funding.output_index as u16,
        }).unwrap();
        let spending_txns: Vec<TxnHeight> = self.load_txns(
            self.store
                .scan(&spend_key)
                .iter()
                .map(|row| {
                    bincode::deserialize::<TxInRow>(&row.key)
                        .unwrap()
                        .txid_prefix
                })
                .collect(),
        );
        let mut spending_inputs = Vec::new();
        for t in &spending_txns {
            for (index, input) in enumerate(&t.txn.input) {
                if input.prev_hash == funding.txn_id
                    && input.prev_index == funding.output_index as u32
                {
                    spending_inputs.push(SpendingInput {
                        txn_id: t.txn.txid(),
                        height: t.height,
                        input_index: index,
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

    pub fn status(&self, script_hash: &[u8]) -> Status {
        let mut status = Status {
            balance: 0,
            funding: vec![],
            spending: vec![],
        };

        let funding_txns = self.load_txns(
            self.store
                .scan(&[b"O", &script_hash[..HASH_PREFIX_LEN]].concat())
                .iter()
                .map(|row| {
                    bincode::deserialize::<TxOutRow>(&row.key)
                        .unwrap()
                        .txid_prefix
                })
                .collect(),
        );
        for t in funding_txns {
            let txn_id = t.txn.txid();
            for (index, output) in enumerate(&t.txn.output) {
                if compute_script_hash(&output.script_pubkey[..]) == script_hash {
                    status.funding.push(FundingOutput {
                        txn_id: txn_id,
                        height: t.height,
                        output_index: index,
                        value: output.value,
                    })
                }
            }
        }
        for funding_output in &status.funding {
            if let Some(spent) = self.find_spending_input(&funding_output) {
                status.spending.push(spent);
            } else {
                status.balance += funding_output.value;
            }
        }
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
}
