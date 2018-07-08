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

First index sync should take ~2.5 hours:
```bash
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
```

The index database is stored here:
```bash
$ du db/
36G db/mainnet/
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
