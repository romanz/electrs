use anyhow::{Context, Result};
use serde::ser::{Serialize, SerializeSeq, Serializer};
use serde_json::json;

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    iter::FromIterator,
    ops::Bound,
    time::Instant,
};

use electrs_index::{
    bitcoin::{
        consensus::deserialize, hashes::hex::FromHex, Amount, OutPoint, Script, Transaction, Txid,
    },
    bitcoincore_rpc::{
        json::{GetMempoolEntryResult, GetTxOutResult},
        RpcApi,
    },
    Daemon, ScriptHash, {Gauge, GaugeVec, Histogram, Metrics},
};

pub struct MempoolEntry {
    pub tx: Transaction,
    pub txid: Txid,
    pub fee: Amount,
    pub vsize: u64,
    scripthashes: HashSet<ScriptHash>, // inputs & outputs
}

#[derive(Debug)]
pub struct HistogramEntry {
    fee_rate: u64,
    vsize: u64,
}

impl HistogramEntry {
    fn new(fee_rate: Amount, vsize: u64) -> Self {
        Self {
            fee_rate: fee_rate.as_sat(),
            vsize,
        }
    }
}

impl Serialize for HistogramEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(2))?;
        seq.serialize_element(&self.fee_rate)?;
        seq.serialize_element(&self.vsize)?;
        seq.end()
    }
}

struct Stats {
    update_duration: Histogram,
    lookup_duration: Histogram,
    tx_rates: FeeRatesGauge,
    tx_count: Gauge,
}

impl Stats {
    fn new(metrics: &Metrics) -> Self {
        Self {
            update_duration: metrics.histogram_vec(
                "mempool_update_duration",
                "Update duration (in seconds)",
                &["step"],
            ),
            lookup_duration: metrics.histogram_vec(
                "mempool_lookup_duration",
                "Lookup duration (in seconds)",
                &["step"],
            ),
            tx_rates: FeeRatesGauge::new(metrics.gauge_vec(
                "mempool_tx_rates",
                "Mempool transactions' vsize",
                "fee_rate",
            )),
            tx_count: metrics.gauge("mempool_tx_count", "Mempool transactions' count"),
        }
    }
}

pub struct Mempool {
    entries: HashMap<Txid, MempoolEntry>,
    by_scripthash: BTreeSet<(ScriptHash, Txid)>,

    // sorted by descending fee rate.
    // vsize of transactions paying >= fee_rate
    histogram: Vec<HistogramEntry>,

    stats: Stats,

    txid_min: Txid,
    txid_max: Txid,
}

impl Mempool {
    pub fn empty(metrics: &Metrics) -> Self {
        Self {
            entries: HashMap::new(),
            by_scripthash: BTreeSet::new(),
            histogram: vec![],

            stats: Stats::new(metrics),

            txid_min: Txid::from_hex(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
            txid_max: Txid::from_hex(
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            )
            .unwrap(),
        }
    }

    pub fn get(&self, txid: &Txid) -> Option<&MempoolEntry> {
        self.stats
            .lookup_duration
            .observe_duration("get", || self.entries.get(txid))
    }

    pub fn lookup(&self, scripthash: ScriptHash) -> Vec<&MempoolEntry> {
        self.stats.lookup_duration.observe_duration("lookup", || {
            let range = (
                Bound::Included((scripthash, self.txid_min)),
                Bound::Included((scripthash, self.txid_max)),
            );
            self.by_scripthash
                .range(range)
                .map(|(_, txid)| self.entries.get(txid).expect("missing entry"))
                .collect()
        })
    }

    pub fn update(&mut self, daemon: &Daemon) -> Result<()> {
        let start = Instant::now();
        let update_duration = self.stats.update_duration.clone();
        let (old, new) = update_duration.observe_duration("poll", || -> Result<_> {
            let old: HashSet<Txid> = self.entries.keys().copied().collect();
            let new: HashSet<Txid> = daemon
                .client()
                .get_raw_mempool()
                .context("get_raw_mempool failed")?
                .into_iter()
                .collect();
            Ok((old, new))
        })?;
        let removed = update_duration.observe_duration("remove", || {
            // removed entries
            let removed = &old - &new;
            let removed_len = removed.len();
            for txid in removed {
                let entry = self.entries.remove(&txid).expect("missing entry");
                for scripthash in &entry.scripthashes {
                    assert!(
                        self.by_scripthash.remove(&(*scripthash, txid)),
                        "missing entry"
                    );
                }
            }
            removed_len
        });
        let entries = update_duration.observe_duration("fetch", || -> Result<_> {
            // added entries
            get_mempool_entries(daemon, &new - &old, &update_duration)
        })?;
        let added = entries.len();
        update_duration.observe_duration("add", || {
            for entry in entries {
                for scripthash in &entry.scripthashes {
                    self.by_scripthash.insert((*scripthash, entry.txid));
                }
                assert!(self.entries.insert(entry.txid, entry).is_none());
            }
        });
        update_duration.observe_duration("histogram", || {
            self.histogram = build_histogram(self.entries.values(), &mut self.stats.tx_rates);
        });
        self.stats.tx_count.set(self.entries.len() as f64);
        debug!(
            "mempool has {} txs: added {}, removed {} (took {} ms)",
            self.entries.len(),
            added,
            removed,
            start.elapsed().as_millis()
        );
        Ok(())
    }

    pub fn histogram(&self) -> &[HistogramEntry] {
        &self.histogram
    }
}

struct FeeRatesIter {
    values: Vec<u64>,
    factor: u64,
    index: usize,
}

impl FeeRatesIter {
    fn new() -> Self {
        Self {
            values: vec![1, 2, 5],
            factor: 10,
            index: 0,
        }
    }
}

impl Iterator for FeeRatesIter {
    type Item = Amount;

    fn next(&mut self) -> Option<Amount> {
        if self.index == self.values.len() {
            self.index = 0;
            for v in &mut self.values {
                *v *= self.factor
            }
        }
        let result = self.values[self.index];
        self.index += 1;
        Some(Amount::from_sat(result))
    }
}

struct FeeRatesGauge {
    gauge: GaugeVec,
    max_limit: Amount,
}

impl FeeRatesGauge {
    fn new(gauge: GaugeVec) -> Self {
        Self {
            gauge,
            max_limit: Amount::ZERO,
        }
    }

    fn observe_histogram(&mut self, fee_rates: &[(Amount, u64)]) {
        let mut limit_iter = FeeRatesIter::new().peekable();
        let mut total_vsize = 0.0;
        let mut prev_limit = Amount::ZERO;

        for (rate, vsize) in fee_rates {
            loop {
                let limit = limit_iter.peek().unwrap();
                if rate <= limit {
                    break;
                }
                self.gauge
                    .with_label_values(&[&format!(
                        "{:10} - {:10}",
                        prev_limit.as_sat(),
                        limit.as_sat()
                    )])
                    .set(total_vsize);
                total_vsize = 0.0;
                prev_limit = *limit;
                limit_iter.next();
            }
            total_vsize += *vsize as f64;
        }
        loop {
            let limit = limit_iter.peek().unwrap();
            self.gauge
                .with_label_values(&[&format!(
                    "{:10} - {:10}",
                    prev_limit.as_sat(),
                    limit.as_sat()
                )])
                .set(total_vsize);
            total_vsize = 0.0;
            prev_limit = *limit;
            if *limit >= self.max_limit {
                break;
            }
            limit_iter.next();
        }
        self.max_limit = *limit_iter.peek().unwrap();
    }
}

fn build_histogram<'a>(
    entries: impl Iterator<Item = &'a MempoolEntry>,
    gauge: &mut FeeRatesGauge,
) -> Vec<HistogramEntry> {
    let mut fee_rates = HashMap::<Amount, u64>::new();
    for e in entries {
        let rate = e.fee / e.vsize;
        let total_vsize = fee_rates.entry(rate).or_default();
        *total_vsize += e.vsize;
    }
    let mut fee_rates = Vec::from_iter(fee_rates); // fee rates should be unique now

    fee_rates.sort_by_key(|item| item.0);
    gauge.observe_histogram(&fee_rates);

    let mut histogram = vec![];
    let mut bin_vsize = 0;
    let mut bin_fee_rate = Amount::ZERO;
    for (fee_rate, vsize) in fee_rates.into_iter().rev() {
        bin_fee_rate = fee_rate;
        bin_vsize += vsize;
        if bin_vsize > 100_000 {
            // `vsize` transactions are paying >= `fee_rate`.
            histogram.push(HistogramEntry::new(bin_fee_rate, bin_vsize));
            bin_vsize = 0;
        }
    }
    if bin_vsize > 0 {
        histogram.push(HistogramEntry::new(bin_fee_rate, bin_vsize));
    }
    histogram
}

fn get_entries_map(
    daemon: &Daemon,
    txids: &HashSet<Txid>,
) -> Result<HashMap<Txid, GetMempoolEntryResult>> {
    let client = daemon.client().get_jsonrpc_client();
    let txids: Vec<Txid> = txids.iter().copied().collect();
    let args_vec: Vec<Vec<_>> = txids.iter().map(|txid| vec![json!(txid)]).collect();

    let responses: Vec<_> = {
        let requests: Vec<_> = args_vec
            .iter()
            .map(|args| client.build_request("getmempoolentry", args))
            .collect();
        client
            .send_batch(&requests)
            .with_context(|| format!("failed to fetch {} mempool entries", requests.len()))
    }?;
    let entries: Vec<_> = responses
        .into_iter()
        .map(|r| {
            r.expect("no response")
                .into_result::<GetMempoolEntryResult>()
                .context("no mempool entry")
        })
        .collect::<Result<_>>()?;
    trace!("got {} mempool entries", entries.len());
    assert_eq!(entries.len(), args_vec.len());
    Ok(txids.into_iter().zip(entries.into_iter()).collect())
}

fn get_transactions_map(
    daemon: &Daemon,
    txids: &HashSet<Txid>,
) -> Result<HashMap<Txid, Transaction>> {
    let client = daemon.client().get_jsonrpc_client();
    let txids: Vec<Txid> = txids.iter().copied().collect();
    let args_vec: Vec<Vec<_>> = txids.iter().map(|txid| vec![json!(txid)]).collect();

    let responses: Vec<_> = {
        let requests: Vec<_> = args_vec
            .iter()
            .map(|args| client.build_request("getrawtransaction", args))
            .collect();
        client
            .send_batch(&requests)
            .with_context(|| format!("failed to fetch {} mempool transactions", requests.len()))
    }?;
    let txs: Vec<Transaction> = responses
        .into_iter()
        .map(|r| {
            let hex = r
                .expect("no response")
                .into_result::<String>()
                .context("no transaction")?;
            let bytes: Vec<u8> = FromHex::from_hex(&hex).expect("invalid hex");
            Ok(deserialize(&bytes).expect("invalid transaction"))
        })
        .collect::<Result<_>>()?;
    trace!("got {} mempool transactions", txs.len());
    assert_eq!(txs.len(), txids.len());
    Ok(txids.into_iter().zip(txs.into_iter()).collect())
}

fn get_confirmed_outpoints(
    daemon: &Daemon,
    outpoints: Vec<OutPoint>,
) -> Result<HashMap<OutPoint, ScriptHash>> {
    if outpoints.is_empty() {
        return Ok(HashMap::new());
    }
    let client = daemon.client().get_jsonrpc_client();
    let args_vec: Vec<Vec<_>> = outpoints
        .iter()
        .map(|outpoint| vec![json!(outpoint.txid), json!(outpoint.vout), json!(false)])
        .collect();

    let responses: Vec<_> = {
        let requests: Vec<_> = args_vec
            .iter()
            .map(|args| client.build_request("gettxout", args))
            .collect();
        client
            .send_batch(&requests)
            .with_context(|| format!("failed to fetch {} UTXOs", requests.len()))
    }?;
    let scripthashes: Vec<ScriptHash> = responses
        .into_iter()
        .map(|r| {
            let utxo = r
                .expect("no response")
                .into_result::<GetTxOutResult>()
                .context("missing UTXO")?;
            Ok(ScriptHash::new(&Script::from(utxo.script_pub_key.hex)))
        })
        .collect::<Result<_>>()?;
    trace!("got {} confirmed scripthashes", scripthashes.len());
    assert_eq!(scripthashes.len(), outpoints.len());
    Ok(outpoints
        .into_iter()
        .zip(scripthashes.into_iter())
        .collect())
}

fn get_mempool_entries(
    daemon: &Daemon,
    mut txids: HashSet<Txid>,
    update_duration: &Histogram,
) -> Result<Vec<MempoolEntry>> {
    if txids.is_empty() {
        return Ok(vec![]);
    }
    let entry_map =
        update_duration.observe_duration("fetch_entries", || get_entries_map(daemon, &txids))?;
    for e in entry_map.values() {
        for txid in &e.depends {
            txids.insert(*txid);
        }
    }
    let mut unconfirmed_map: HashMap<Txid, Transaction> = update_duration
        .observe_duration("fetch_transactions", || {
            get_transactions_map(daemon, &txids)
        })?;

    let mut confirmed_inputs = vec![];
    let mut unconfirmed_inputs = vec![];
    for (txid, entry) in &entry_map {
        let tx = unconfirmed_map.get(txid).expect("missing mempool tx");
        for txi in &tx.input {
            if entry.depends.contains(&txi.previous_output.txid) {
                unconfirmed_inputs.push(txi.previous_output)
            } else {
                confirmed_inputs.push(txi.previous_output)
            }
        }
    }
    let mut scripthashes_map = update_duration.observe_duration("fetch_confirmed", || {
        get_confirmed_outpoints(daemon, confirmed_inputs)
    })?;
    for prev in unconfirmed_inputs {
        let prev_tx = unconfirmed_map.get(&prev.txid).expect("missing mempool tx");
        let prev_out = &prev_tx.output[prev.vout as usize];
        scripthashes_map.insert(prev, ScriptHash::new(&prev_out.script_pubkey));
    }

    Ok(entry_map
        .into_iter()
        .map(|(txid, entry)| {
            let tx = unconfirmed_map.remove(&txid).expect("missing mempool tx");
            let scripthashes = tx
                .output
                .iter()
                .map(|txo| ScriptHash::new(&txo.script_pubkey))
                .chain(tx.input.iter().map(|txi| {
                    scripthashes_map
                        .remove(&txi.previous_output)
                        .expect("missing input")
                }))
                .collect();
            MempoolEntry {
                tx,
                txid,
                fee: entry.fees.base,
                vsize: entry.vsize,
                scripthashes,
            }
        })
        .collect())
}
