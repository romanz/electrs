## Installation

Install [latest Rust](https://rustup.rs/) (1.26+) and [latest Bitcoin Core](https://bitcoincore.org/en/download/) (0.16+).

Also, install `clang` (for building `rust-rocksdb`):
```bash
$ sudo apt update
$ sudo apt install clang
```

## Build

First build should take ~20 minutes:
```bash
$ cargo build --release
```


## Bitcoind configuration

Allow Bitcoin daemon to sync before starting Electrum server:
```bash
$ bitcoind -server=1 -daemon=0 -txindex=0 -prune=0
```

If you are using `-rpcuser=USER` and `-rpcpassword=PASSWORD` for authentication, please use `--cookie=USER:PASSWORD` command-line flag.
Otherwise, [`~/.bitcoin/.cookie`](https://github.com/bitcoin/bitcoin/blob/0212187fc624ea4a02fc99bc57ebd413499a9ee1/contrib/debian/examples/bitcoin.conf#L70-L72) will be read, allowing this server to use bitcoind JSONRPC interface.

## Usage

First index sync should take ~2 hours:
```bash
$ cargo run --release -- -vvv --timestamp --db-dir ./db
2018-07-12T21:30:46 - DEBUG - BlockchainInfo { chain: "main", blocks: 531645, headers: 531645, bestblockhash: "00000000000000000006e41b275b21fc44e3b7afa8a8092aa6a7e4b84345f1f1", size_on_disk: 199667678141, pruned: false }
2018-07-12T21:30:46 - DEBUG - opening "./db/mainnet" with StoreOptions { bulk_import: true }
2018-07-12T21:30:46 - INFO - indexing 1313 blk*.dat files
2018-07-12T21:30:58 - DEBUG - applying 531646 new headers from height 0
2018-07-12T22:34:36 - DEBUG - last indexed block: best=00000000000000000006e41b275b21fc44e3b7afa8a8092aa6a7e4b84345f1f1 height=531645 @ 2018-07-12T18:20:51Z
2018-07-12T22:34:37 - INFO - starting full compaction
2018-07-12T23:18:53 - INFO - finished full compaction
2018-07-12T23:18:53 - DEBUG - opening "./db/mainnet" with StoreOptions { bulk_import: false }
2018-07-12T23:19:05 - DEBUG - applying 531646 new headers from height 0
2018-07-12T23:19:05 - DEBUG - downloading new block headers (531646 already indexed) from 0000000000000000002face92073e7b1dbcb02df32ea891b187a6c10b37dc8ad
2018-07-12T23:19:06 - INFO - best=0000000000000000002face92073e7b1dbcb02df32ea891b187a6c10b37dc8ad height=531656 @ 2018-07-12T20:07:12Z (11 left to index)
2018-07-12T23:19:08 - DEBUG - applying 11 new headers from height 531646
2018-07-12T23:19:11 - INFO - RPC server running on 127.0.0.1:50001
```

The index database is stored here:
```bash
$ du db/
37G db/mainnet/
```

## Electrum client
```bash
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
