use std::collections::HashSet;

use bitcoin::Txid;

use crate::chain::Transaction;
use crate::new_index::{db::DBRow, ChainQuery};
use crate::util::BlockId;

#[derive(Serialize, Deserialize, Debug)]
pub struct TxPegInfo {
    pub txid: Txid,
    peg_in_amount: u64,
    peg_out_amount: u64,
}

impl TxPegInfo {
    fn get_info(tx: &Transaction) -> Option<Self> {
        let (peg_in_amount, peg_out_amount) = peg_amounts(tx);

        if peg_in_amount > 0 || peg_out_amount > 0 {
            Some(TxPegInfo {
                txid: tx.txid(),
                peg_in_amount,
                peg_out_amount,
            })
        } else {
            None
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TxPegRow {
    key: TxPegKey,
}

#[derive(Serialize, Deserialize, Debug)]
struct TxPegKey {
    code: u8,
    confirmed_height: u32,
    peginfo: TxPegInfo,
}

impl TxPegRow {
    fn new(confirmed_height: u32, peginfo: TxPegInfo) -> Self {
        TxPegRow {
            key: TxPegKey {
                code: b'P',
                confirmed_height,
                peginfo,
            },
        }
    }

    pub fn from_row(row: DBRow) -> Self {
        TxPegRow {
            key: bincode::config()
                .big_endian()
                .deserialize(&row.key)
                .expect("failed to parse TxPegKey"),
        }
    }

    pub fn txid(&self) -> Txid {
        self.key.peginfo.txid
    }

    fn into_row(&self) -> DBRow {
        // must use big endian for correct confirmed_height sorting
        DBRow {
            key: bincode::config().big_endian().serialize(&self.key).unwrap(),
            value: vec![],
        }
    }
}

// Index confirmed pegins/pegouts to db rows
pub fn index_confirmed_tx_pegs(tx: &Transaction, confirmed_height: u32, rows: &mut Vec<DBRow>) {
    if let Some(peginfo) = TxPegInfo::get_info(tx) {
        debug!(
            "indexing confirmed tx peg at height {}: {:?}",
            confirmed_height, peginfo
        );
        rows.push(TxPegRow::new(confirmed_height, peginfo).into_row());
    }
}

pub fn lookup_confirmed_tx_pegs_history(
    chain: &ChainQuery,
    last_seen_txid: Option<&Txid>,
    limit: usize,
) -> Vec<(Transaction, BlockId)> {
    let prefix = b"P";
    let prefix_max = bincode::serialize(&(b'P', std::u32::MAX)).unwrap();

    let txs_conf = chain
        .iter_scan_reverse(prefix, &prefix_max)
        .map(TxPegRow::from_row)
        // XXX expose peg in/out amonut information?
        .map(|row| row.txid())
        // TODO seek directly to last seen tx without reading earlier rows
        .skip_while(|txid| {
            // skip until we reach the last_seen_txid
            last_seen_txid.map_or(false, |last_seen_txid| last_seen_txid != txid)
        })
        .skip(match last_seen_txid {
            Some(_) => 1, // skip the last_seen_txid itself
            None => 0,
        })
        .filter_map(|txid| chain.tx_confirming_block(&txid).map(|b| (txid, b)))
        .take(limit)
        .collect::<Vec<(Txid, BlockId)>>();

    chain
        .lookup_txns(&txs_conf)
        .expect("failed looking up txs in peg history index")
        .into_iter()
        .zip(txs_conf)
        .map(|(tx, (_, blockid))| (tx, blockid))
        .collect()
}

// Index mempool transaction pegins/pegouts to in-memory store
pub fn index_mempool_tx_pegs(tx: &Transaction, pegs_history: &mut Vec<TxPegInfo>) {
    if let Some(peginfo) = TxPegInfo::get_info(tx) {
        pegs_history.push(peginfo);
    }
}

// Remove mempool peg in/out transaction from in-memory store
pub fn remove_mempool_tx_pegs(to_remove: &HashSet<&Txid>, pegs_history: &mut Vec<TxPegInfo>) {
    // TODO optimize
    pegs_history.retain(|peginfo| !to_remove.contains(&peginfo.txid));
}

fn peg_amounts(tx: &Transaction) -> (u64, u64) {
    // XXX check the peg in/out asset type is explicit and matches the sidechain's native token?
    (
        tx.input
            .iter()
            .filter_map(|txin| txin.pegin_data())
            .map(|pegin| pegin.value)
            .sum(),
        tx.output
            .iter()
            .filter_map(|txout| txout.pegout_data())
            .map(|pegout| pegout.value)
            .sum(),
    )
}
