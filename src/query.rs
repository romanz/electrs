use bincode;
use bitcoin::blockdata::transaction::Transaction;
use bitcoin::network::serialize::deserialize;
use bitcoin::util::hash::Sha256dHash;
use itertools::enumerate;

use daemon::Daemon;
use index::{compute_script_hash, hash_prefix, HashPrefix, TxInKey, TxInRow, TxKey, TxOutRow,
            HASH_PREFIX_LEN};
use store::Store;

pub struct Query<'a> {
    store: &'a Store,
    daemon: &'a Daemon,
}

impl<'a> Query<'a> {
    pub fn new(store: &'a Store, daemon: &'a Daemon) -> Query<'a> {
        Query { store, daemon }
    }

    fn load_txns(&self, prefixes: Vec<HashPrefix>) -> Vec<Transaction> {
        let mut txns = Vec::new();
        for txid_prefix in prefixes {
            for row in self.store.scan(&[b"T", &txid_prefix[..]].concat()) {
                let key: TxKey = bincode::deserialize(&row.key).unwrap();
                let txid: Sha256dHash = deserialize(&key.txid).unwrap();
                let txn_bytes = self.daemon.get(&format!("tx/{}.bin", txid.be_hex_string()));
                let txn: Transaction = deserialize(&txn_bytes).unwrap();
                txns.push(txn)
            }
        }
        txns
    }

    fn find_spending_txn(&self, txid: &Sha256dHash, output_index: u32) -> Option<Transaction> {
        let spend_key = bincode::serialize(&TxInKey {
            code: b'I',
            prev_hash_prefix: hash_prefix(&txid[..]),
            prev_index: output_index as u16,
        }).unwrap();
        let mut spending: Vec<Transaction> = self.load_txns(
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
        spending.retain(|item| {
            item.input
                .iter()
                .any(|input| input.prev_hash == *txid && input.prev_index == output_index)
        });
        assert!(spending.len() <= 1);
        if spending.len() == 1 {
            Some(spending.remove(0))
        } else {
            None
        }
    }

    pub fn balance(&self, script_hash: &[u8]) -> f64 {
        let mut funding: Vec<Transaction> = self.load_txns(
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
        funding.retain(|item| {
            item.output
                .iter()
                .any(|output| compute_script_hash(&output.script_pubkey[..]) == script_hash)
        });

        let mut balance = 0u64;
        let mut spending = Vec::<Transaction>::new();
        for txn in &funding {
            let txid = txn.txid();
            for (index, output) in enumerate(&txn.output) {
                if let Some(spent) = self.find_spending_txn(&txid, index as u32) {
                    spending.push(spent); // TODO: may contain duplicate TXNs
                } else {
                    balance += output.value;
                }
            }
        }
        balance as f64 / 100_000_000f64
    }
}
