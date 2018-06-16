# Electrum Server in Rust

[![Build Status](https://travis-ci.com/romanz/electrs.svg?branch=master)](https://travis-ci.com/romanz/electrs)

An efficient re-implementation of Electrum Server, inspired by [ElectrumX](https://github.com/kyuupichan/electrumx)
and [Electrum Personal Server](https://github.com/chris-belcher/electrum-personal-server/).

## Features:

 * Supports Electrum protocol [v1.2](https://electrumx.readthedocs.io/en/latest/protocol.html).
 * Maintains an index over transaction inputs and outputs, allowing fast balance queries.
 * Fast synchronization of the Bitcoin blockchain (~5 hours for ~184GB @ June 2018) on modest hardware (without SSD).
 * Low index storage overhead (~20%), relying on a local full node for actual transaction retrieval.
 * Efficient mempool tracker (allowing better fee estimation).
 * Low CPU & memory usage after initial indexing is over.
 * [`txindex`](https://github.com/bitcoin/bitcoin/blob/81069a75bd71f21f9cbab97c68f7347073cc9ae5/src/init.cpp#L406) is not required for the Bitcoin node.
 * Using a single RocksDB database, for better consistency and crash recovery.

## Usage

Install [latest Rust](https://rustup.rs/) (1.26+) and [latest Bitcoin Core](https://bitcoincore.org/en/download/) (0.16+).

```bash
$ sudo apt update
$ sudo apt install clang

# Allow Bitcoin daemon to sync before starting Electrum server
$ bitcoind -server=1 -daemon=0 -txindex=0 -prune=0

# First build should take ~20 minutes
$ cargo build --release
$ cargo run --release -- -v -l debug.log
Config { log_file: "debug.log", log_level: Debug, network_type: Mainnet, db_path: "./db/mainnet", rpc_addr: V4(127.0.0.1:50001), monitoring_addr: V4(127.0.0.1:42024) }
BlockchainInfo { chain: "main", blocks: 527673, headers: 527677, bestblockhash: "0000000000000000001134b741f53f4e49e9f8073e41af6d8aaad3b849ebeee4", size_on_disk: 196048138442, pruned: false }
opening ./db/mainnet with StoreOptions { bulk_import: true }
applying 0 new headers from height 0
best=0000000000000000001134b741f53f4e49e9f8073e41af6d8aaad3b849ebeee4 height=527673 @ 2018-06-16T04:03:53Z (527674 left to index)
# <initial indexing takes a few hours>
applying 527674 new headers from height 0
closing ./db/mainnet
opening ./db/mainnet with StoreOptions { bulk_import: false }
RPC server running on 127.0.0.1:50001

# The index database is stored here:
$ du db/
36G db/mainnet/

# Connect only to the local server, for better privacy
$ electrum  --oneserver --server=127.0.0.1:50001:t
```

## Monitoring

Indexing and serving metrics are exported via [Prometheus](https://github.com/pingcap/rust-prometheus):

```bash
$ sudo apt install prometheus
$ echo "
scrape_configs:
  - job_name: electrs
    static_configs:
    - targets: ['localhost:42024']
" | sudo tee -a /etc/prometheus/prometheus.yml
$ sudo systemctl restart prometheus
$ firefox 'http://localhost:9090/graph?g0.range_input=1h&g0.expr=index_height&g0.tab=0'
```
