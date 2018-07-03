# Electrum Server in Rust

[![Build Status](https://travis-ci.com/romanz/electrs.svg?branch=master)](https://travis-ci.com/romanz/electrs)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg?style=flat-square)](http://makeapullrequest.com)
[![crates.io](http://meritbadge.herokuapp.com/electrs)](https://crates.io/crates/electrs)

An efficient re-implementation of Electrum Server, inspired by [ElectrumX](https://github.com/kyuupichan/electrumx), [Electrum Personal Server](https://github.com/chris-belcher/electrum-personal-server) and [bitcoincore-indexd](https://github.com/jonasschnelli/bitcoincore-indexd).

The motivation behind this project is to enable a user to run his own Electrum server,
with required hardware resources not much beyond those of a [full node](https://en.bitcoin.it/wiki/Full_node#Why_should_you_use_a_full_node_wallet).
The server indexes the entire Bitcoin blockchain, and the resulting index enables fast queries for any given user wallet,
allowing the user to keep real-time track of his balances and his transaction history using the [Electrum wallet](https://electrum.org/).
Since it runs on the user's own machine, there is no need for the wallet to communicate with external Electrum servers,
thus preserving the privacy of the user's addresses and balances.

## Features

 * Supports Electrum protocol [v1.2](https://electrumx.readthedocs.io/en/latest/protocol.html)
 * Maintains an index over transaction inputs and outputs, allowing fast balance queries
 * Fast synchronization of the Bitcoin blockchain (~2.5 hours for ~185GB @ June 2018) on [modest hardware](https://gist.github.com/romanz/cd9324474de0c2f121198afe3d063548)
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

# First index sync should take ~2.5 hours
$ cargo run --release -- -vvv --timestamp --db-dir ./db
2018-06-28T23:09:17 - DEBUG - BlockchainInfo { chain: "main", blocks: 529656, headers: 529656, bestblockhash: "0000000000000000000d6344eeaa8dece87a438c25948e9038e8fecd4c64ac0f", size_on_disk: 197723753341, pruned: false }
2018-06-28T23:09:17 - DEBUG - opening ./db/mainnet with StoreOptions { bulk_import: true }
2018-06-28T23:09:30 - INFO - indexing 1300 blk*.dat files
2018-06-29T00:28:16 - DEBUG - read 1300 blk files
2018-06-29T00:28:22 - INFO - indexed 529657 blocks
2018-06-29T00:28:23 - INFO - starting full compaction
2018-06-29T01:35:02 - INFO - finished full compaction
2018-06-29T01:35:02 - DEBUG - closing ./db/mainnet
2018-06-29T01:35:03 - DEBUG - opening ./db/mainnet with StoreOptions { bulk_import: false }
2018-06-29T01:35:12 - DEBUG - applying 529657 new headers from height 0
2018-06-29T01:35:13 - INFO - RPC server running on 127.0.0.1:50001
2018-06-29T01:35:14 - DEBUG - downloading new block headers (529657 already indexed) from 000000000000000000207ca53fd49f8de7f7f67dcde34af505882ab2be5d8fc5
2018-06-29T01:35:14 - INFO - best=000000000000000000207ca53fd49f8de7f7f67dcde34af505882ab2be5d8fc5 height=529668 @ 2018-06-28T22:26:05Z (12 left to index)
2018-06-29T01:35:15 - DEBUG - applying 12 new headers from height 529657

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

## Index database

The database schema is described [here](doc/schema.md).
