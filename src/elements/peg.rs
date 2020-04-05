use std::collections::HashSet;

use bitcoin::{hashes::hex::ToHex, BlockHash, Script, Txid};
use elements::TxOut;

use crate::chain::{Network, Transaction};
use crate::new_index::{db::DBRow, ChainQuery, Mempool, Query};
use crate::util::{get_script_asm, script_to_address, BlockId};

// API representation of pegout data assocaited with an output
#[derive(Serialize, Deserialize, Clone)]
pub struct PegoutValue {
    pub genesis_hash: String,
    pub scriptpubkey: Script,
    pub scriptpubkey_asm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scriptpubkey_address: Option<String>,
}

impl PegoutValue {
    pub fn parse(txout: &TxOut, parent_network: Network) -> Option<Self> {
        let pegoutdata = txout.pegout_data()?;

        if pegoutdata.genesis_hash != parent_network.genesis_hash() {
            return None;
        }

        Some(PegoutValue {
            genesis_hash: pegoutdata.genesis_hash.to_hex(),
            scriptpubkey_asm: get_script_asm(&pegoutdata.script_pubkey),
            scriptpubkey_address: script_to_address(&pegoutdata.script_pubkey, parent_network),
            scriptpubkey: pegoutdata.script_pubkey,
        })
    }
}

//
// Indexer
//

// Indexer representation of tx peg statistics
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

// DB representation of confirmed peg
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
    let prefix_max = bincode::serialize(&(b'P', std::u32::MAX)).unwrap();

    let txs_conf = chain
        .iter_scan_reverse(b"P", &prefix_max)
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

//
// Peg stats
//

#[derive(Serialize, Deserialize, Debug)]
pub struct PegStats {
    pub tx_count: usize,
    pub peg_in_amount: u64,
    pub peg_out_amount: u64,
}

impl PegStats {
    fn default() -> Self {
        Self {
            tx_count: 0,
            peg_in_amount: 0,
            peg_out_amount: 0,
        }
    }
}

pub fn peg_stats(query: &Query) -> (PegStats, PegStats) {
    (
        chain_peg_stats(query.chain()),
        mempool_peg_stats(&query.mempool()),
    )
}

fn chain_peg_stats(chain: &ChainQuery) -> PegStats {
    // get the last known stats and the blockhash they are updated for.
    // invalidates the cache if the block was orphaned.
    let cache: Option<(PegStats, usize)> = chain
        .store()
        .cache_db()
        .get(b"p")
        .map(|c| bincode::deserialize(&c).unwrap())
        .and_then(|(stats, blockhash)| {
            chain
                .height_by_hash(&blockhash)
                .map(|height| (stats, height))
        });

    debug!("loaded peg cache: {:?}", cache);

    // update stats with new transactions since
    let (newstats, lastblock) = cache.map_or_else(
        || peg_stats_delta(chain, PegStats::default(), 0),
        |(oldstats, blockheight)| peg_stats_delta(chain, oldstats, blockheight + 1),
    );

    debug!("new peg chain stats at {:?}: {:?}", lastblock, newstats);

    // save updated stats to cache
    if let Some(lastblock) = lastblock {
        let new_cache = bincode::serialize(&(&newstats, lastblock)).unwrap();
        chain.store().cache_db().put_sync(b"p", &new_cache);
    }

    newstats
}

fn peg_stats_delta(
    chain: &ChainQuery,
    init_stats: PegStats,
    start_height: usize,
) -> (PegStats, Option<BlockHash>) {
    let start_at = bincode::config()
        .big_endian()
        .serialize(&(b'P', start_height as u32))
        .unwrap();

    let history_iter = chain
        .iter_scan(b"P", &start_at)
        .map(TxPegRow::from_row)
        .filter_map(|row| {
            chain
                .tx_confirming_block(&row.txid())
                .map(|blockid| (row, blockid))
        });

    let mut stats = init_stats;
    let mut lastblock = None;

    debug!("applying new stats delta since {}", start_height);
    for (row, blockid) in history_iter {
        trace!("applying from block {:?}: {:?}", blockid, row);
        apply_peg_stats(&row.key.peginfo, &mut stats);
        lastblock = Some(blockid.hash);
    }

    (stats, lastblock)
}

pub fn mempool_peg_stats(mempool: &Mempool) -> PegStats {
    let mut stats = PegStats::default();
    for info in &mempool.pegs_history {
        apply_peg_stats(&info, &mut stats);
    }
    stats
}

fn apply_peg_stats(info: &TxPegInfo, stats: &mut PegStats) {
    stats.tx_count += 1;
    stats.peg_in_amount += info.peg_in_amount;
    stats.peg_out_amount += info.peg_out_amount;
}
