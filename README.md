# Electrum Server in Rust

<p>
  <img src="https://upload.wikimedia.org/wikipedia/commons/c/c3/Gold-121700.jpg" width="40%" />
</p>

[![Build Status](https://travis-ci.com/romanz/electrs.svg?branch=master)](https://travis-ci.com/romanz/electrs)

An efficient re-implementation of Electrum Server, inspired by [ElectrumX](https://github.com/kyuupichan/electrumx)
and [Electrum Personal Server](https://github.com/chris-belcher/electrum-personal-server/).

The motivation behind this project is to enable a user to run his own Electrum server,
with required hardware resources not much beyond those of a [full node](https://en.bitcoin.it/wiki/Full_node#Why_should_you_use_a_full_node_wallet).
The server indexes the entire Bitcoin blockchain, and the resulting index enables fast queries for any given user wallet,
allowing the user to keep real-time track of his balances and his transaction history using the [Electrum wallet](https://electrum.org/).
Since it runs on the user's own machine, there is no need for the wallet to communicate with external Electrum servers,
thus preserving the privacy of the user's addresses and balances.

## Features

 * Supports Electrum protocol [v1.2](https://electrumx.readthedocs.io/en/latest/protocol.html)
 * Maintains an index over transaction inputs and outputs, allowing fast balance queries
 * Fast synchronization of the Bitcoin blockchain (~5 hours for ~184GB @ June 2018) on modest hardware (without SSD)
 * Low index storage overhead (~20%), relying on a local full node for transaction retrieval
 * Efficient mempool tracker (allowing better fee [estimation](https://github.com/spesmilo/electrum/blob/59c1d03f018026ac301c4e74facfc64da8ae4708/RELEASE-NOTES#L34-L46))
 * Low CPU & memory usage (after initial indexing)
 * [`txindex`](https://github.com/bitcoinbook/bitcoinbook/blob/develop/ch03.asciidoc#txindex) is not required for the Bitcoin node
 * Uses a single [RocksDB](https://github.com/spacejam/rust-rocksdb) database, for better consistency and crash recovery

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
[INFO] Config { log_file: "debug.log", log_level: Debug, network_type: Mainnet, db_path: "./db/mainnet", rpc_addr: V4(127.0.0.1:50001), monitoring_addr: V4(127.0.0.1:42024) }
[DEBUG] BlockchainInfo { chain: "main", blocks: 527673, headers: 527677, bestblockhash: "0000000000000000001134b741f53f4e49e9f8073e41af6d8aaad3b849ebeee4", size_on_disk: 196048138442, pruned: false }
[DEBUG] opening ./db/mainnet with StoreOptions { bulk_import: true }
[DEBUG] applying 0 new headers from height 0
[INFO] best=0000000000000000001134b741f53f4e49e9f8073e41af6d8aaad3b849ebeee4 height=527673 @ 2018-06-16T04:03:53Z (527674 left to index)
# <initial indexing takes a few hours>
[DEBUG] applying 527674 new headers from height 0
[INFO] starting full compaction
# <full compaction happens once, and may take ~1 hour>
[INFO] finished full compaction
[DEBUG] closing ./db/mainnet
[DEBUG] opening ./db/mainnet with StoreOptions { bulk_import: false }
[INFO] RPC server running on 127.0.0.1:50001

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
