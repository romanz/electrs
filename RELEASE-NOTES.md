# 0.8.6 (25 Nov 2020)
* [Fix](https://github.com/romanz/electrs/commit/c88a0dc331eb16163276becf98fcc020565d97eb) Electrum fee histogram duplicates
* [Fix](https://github.com/romanz/electrs/commit/8f2f53303a62321e3ccd1a8dc42b46c63629a03f) Electrum protocol negotiation
* Update multiple crates (@kixunil): [lru](#333), [prometheus](#334), [dirs-next](#335)
* Support Rust 1.41.1 (for Debian stable)
* [Update](https://github.com/romanz/electrs/commit/af6ff09a275ec12b6fd0d6a101637f4710902a3c) bitcoin crate (@dr-orlovsky)
* [Fix](https://github.com/romanz/electrs/commit/4764dccbbe4cd04a6dc79771a686847d8e6e2edf) a deadlock when shutting down (@kixunil)

# 0.8.5 (1 July 2020)

* Add a 'blocks_dir' option (@darosior)
* Return fee for unconfirmed transactions history (for Electrum 4.0)
* Handle SIGUSR1 for external notifications

# 0.8.4 (3 June 2020)

* Update to latest rust-bitcoin (@dr-orlovsky)
* Fix deadlock and refactor RPC threading (@Kixunil)

# 0.8.3 (30 Jan 2020)

* Fix memory leak (@champo)

# 0.8.2 (6 Dec 2019)

* Downgrade rust-rocksdb to 0.12.2 (https://github.com/romanz/electrs/issues/193)

# 0.8.1 (20 Nov 2019)

* Allow setting `--cookie-file` path via configuration (@Kixunil)
* Bump rust-rocksdb to 0.13.0, using RockDB 6.2.4

# 0.8.0 (28 Oct 2019)

* Use `configure_me` instead of `clap` to support config files, environment variables and man pages (@Kixunil)
* Don't accept `--cookie` via CLI arguments (@Kixunil)
* Define cache size in MB instead of number of elements (@dagurval)
* Support Rust >=1.34 (for Debian)
* Bump rust-rocksdb to 0.12.3, using RockDB 6.1.2
* Bump bitcoin crate to 0.21 (@MichelKansou)

# 0.7.1 (27 July 2019)

* Allow stopping bulk indexing via SIGINT/SIGTERM
* Cache list of transaction IDs for blocks (@dagurval)

# 0.7.0 (13 June 2019)

* Support Bitcoin Core 0.18
* Build with LTO
* Allow building with latest Rust (via feature flag)
* Use iterators instead of returning vectors (@Kixunil)
* Use atomics instead of `Mutex<u64>` (@Kixunil)
* Better handling invalid blocks (@azuchi)

# 0.6.2 (17 May 2019)

* Support Rust 1.32 (for Debian)

# 0.6.1 (9 May 2019)

* Fix crash during initial sync
* Switch to `signal-hook` crate

# 0.6.0 (29 Apr 2019)

* Update to Rust 1.34
* Prefix Prometheus metrics with 'electrs_'
* Update RocksDB crate to 0.12.1
* Update Bitcoin crate to 0.18
* Support latest bitcoind mempool entry vsize field name
* Fix "chain-trimming" reorgs
* Serve by default on IPv4 localhost

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
