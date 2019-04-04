# 0.6.0 (TBD)

* Prefix Prometheus metrics with 'electrs_'
* Update RocksDB crate to 0.12.1
* Update Bitcoin crate to 0.18
* Support latest bitcoind mempool entry vsize field name

# 0.5.0 (3 Mar 2019)

* Limit query results, to prevent RPC server to get stuck (see `--txid-limit` flag)
* Update RocksDB crate to 0.11
* Update Bitcoin crate to 0.17

# 0.4.3 (23 Dec 2018)

* Support Rust 2018 edition (1.31)
* Upgrade to Electrum protocol 1.4 (from 1.2)
* Let server banner be configurable via command-line flag
* Improve query.get_merkle_proof() performance

# 0.4.2 (22 Nov 2018)

* Update to rust-bitcoin 0.15.1
* Use bounded LRU cache for transaction retrieval
* Support 'server.ping' and partially 'blockchain.block.header' Electrum RPC

# 0.4.1 (14 Oct 2018)

* Don't run full compaction after initial import is over (when using JSONRPC)

# 0.4.0 (22 Sep 2018)

* Optimize for low-memory systems by using different RocksDB settings
* Rename `--skip_bulk_import` flag to `--jsonrpc-import`

# 0.3.2 (14 Sep 2018)

* Optimize block headers processing during startup
* Handle TCP disconnections during long RPCs
* Use # of CPUs for bulk indexing threads
* Update rust-bitcoin to 0.14
* Optimize block headers processing during startup


# 0.3.1 (20 Aug 2018)

* Reconnect to bitcoind only on transient errors
* Poll mempool after transaction broadcasting

# 0.3.0 (14 Aug 2018)

* Optimize for low-memory systems
* Improve compaction performance
* Handle disconnections from bitcoind by retrying
* Make `blk*.dat` ingestion more robust
* Support regtest network
* Support more Electrum RPC methods
* Export more Prometheus metrics (CPU, RAM, file descriptors)
* Add `scripts/run.sh` for building and running `electrs`
* Add some Python tools (as API usage examples)
* Change default Prometheus monitoring ports

# 0.2.0 (14 Jul 2018)

* Allow specifying custom bitcoind data directory
* Allow specifying JSONRPC cookie from commandline
* Improve initial bulk indexing performance
* Support 32-bit systems

# 0.1.0 (2 Jul 2018)

* Announcement: https://lists.linuxfoundation.org/pipermail/bitcoin-dev/2018-July/016190.html
* Published to https://crates.io/electrs and https://docs.rs/electrs
