# 0.9.13 (Mar 31 2023)

* Upgrade dependencies (`bitcoin` & `bitcoincore-rpc` #865, `crossbeam-channel` #854, `serde_json` #855, `tempfile` #850)
* Don't panic on unexpected magic (#861)
* contrib: rename xpub into get_balance (#627)

# 0.9.12 (Feb 25 2023)

* Update dependencies (`serde_json`, `signal-hook`, `electrs-bitcoincore-rpc`, `electrs-rocksdb`)
* Add `CONTRIBUTING.md` (#838)
* Add `blockchain.transaction.id_from_pos` RPC (#836)
* Add `blockchain.scripthash.unsubscribe` RPC (#833)
* Support IPv6 connections (#792)

# 0.9.11 (Jan 5 2023)

* Update dependencies (`secp256k1`, `serde_json`, `configure_me_codegen`, `env_logger`, `electrs-rocksdb`)
* Improve error logging in case of p2p error (#807)

# 0.9.10 (Nov 3 2022)

* Update dependencies (`bitcoin`, `bitcoincore-rpc`, `tiny_http`, `serde_json`, `env_logger`)
* Fix mempool fee rate formatting (#761)
* Allow configuring signet p2p magic (#762, #768)
* Don't panic in case of an invalid block header height (#786)

# 0.9.9 (Jul 12 2022)

* Update dependencies (`anyhow`, `crossbeam-channel`, `crossbeam-utils`, `regex`, `serde`, `serde from `, `serde_json`, `signal-hook`)
* Don't log scripthash (#737)

# 0.9.8 (Jun 3 2022)

* Update dependencies (`serde_json`, `serde`, `bitcoin`, `bitcoincore-rpc`, `rayon`, `log`)
* Support new Electrum release `getbalance` response format (#717)

# 0.9.7 (Apr 30 2022)

* Add build matrix to test all features in CI (#706)
* Install and run cargo-bloat in CI (#705)
* Add guide for other Ubuntu & Debian releases to compile and install librocksdb (#696)
* Update dependencies (`anyhow`, `log`, `crossbeam-channel`, `rayon`)

# 0.9.6 (Mar 4 2022)

* Allow skipping default config files (#686)
* Update dependencies (`anyhow`, `serde-json`)

# 0.9.5 (Feb 4 2022)

* Update dependencies (`serde-json`, `serde`, `tempfile`, `crossbeam-channel`)
* Fix `txid` index collision handling (#653)
* Use `bitcoincore_rpc`'s `getblockchaininfo` implementation (#656)

# 0.9.4 (Dec 30 2021)

* Update dependencies (`anyhow`, `serde`, `serde_json`, `signal-hook`)
* Improve p2p receiving metrics (#633)
* Warn on attempt to connect over SSL (#634)
* Disable `index_lookup_limit` by default (#635)
* Parse p2p messages in a separate thread (#638)
* Add server loop-related metrics

# 0.9.3 (Nov 20 2021)

* Support basic RPC handling during initial sync (#543)
* Update MSRV requirements to Rust 1.48 (#603)
* Bump `env_logger` to 0.9 (#604)
* Monitor RocksDB statistics via Prometheus (#605)
* Don't warn when queried with unsubscribed scripthashes (#609)
* Allow skipping merkle proofs' during subscription (#610)
* Remove `verbose` configuration (#615)
* Add `curl` to `Dockerfile` (#624)

# 0.9.2 (Oct 31 2021)

* Add a feature to ignore default config files (#396)
* Support Rust 1.48.0 and test on Debian 11 (#455)
* Allow multiple scripthashes' subscription in parallel(#464)
* Ignore `cargo audit` warning in `tiny_http` (#575)
* Re-organize and split documentation (#583)
* Use `/electrs:$VERSION/` in p2p 'user-agent' (#585)
* Build rocksdb with conditional SSE support (#595)
* Ignore 'addr' p2p messages (#596)

# 0.9.1 (Oct 19 2021)

* Initialize chain height metric (#515)
* Don't shutdown write-side before all responses are sent back (#523)
* Use p2p protocol to replace waitfornewblock hidden RPC (#526)
* Use correct Prometheus buckets for size and duration (#528)
* Don't ignore signals during IBD (#533)
* Expose index DB size as a Prometheus gauge metric (#544)
* Add p2p protocol monitoring (#546)
* Fix contrib/xpub.py support for ypub/zpub keys (#549)
* Rewrite and simplify p2p message receiving thread (#550)
* Re-introduce mempool vsize and txs' count metrics (#557, #562, #563)
* Allow RPC connection before initial sync is over (#558)

# 0.9.0 (Sep 30 2021)

**IMPORTANT: This release contains major changes, please read carefully!**

The four main things to watch out for:

* Database schema changed - this will cause **reindex after upgrade**.
* `mainnet` subdirectory was renamed to `bitcoin`, you should delete `mainnet` after successful reindex.
* We now use **bitcoin p2p protocol** to fetch blocks - some configurations may not work.
* Trace log level now logs much more information - make sure it's not used in production.

See [upgrading](doc/upgrading.md) section of our docs to learn more.

Full list of changes:

* Add electrs logo (#510)

## 0.9.0-rc2 (Sep 23 2021)

* Prevent panic during logging of p2p messages (#490)
* Don't collect process' Prometheus metrics by default (#492)
* Support initial sync resume (#494)

## 0.9.0-rc1 (Sep 17 2021)

* Fix incorrect ordering of same-block transactions (#297)
* Change DB index format and use Zstd compression (instead of Snappy)
* The database will be reindexed automatically when it encounters old version (#477)
* Don't use bitcoind JSON RPC for fetching blocks (#373)
* Use p2p protocol for headers and block fetching only.
  This is safer than reading `blk*dat` files and faster than JSON RPC.
* Support Electrum JSON RPC batching and errors
* Use `rust-bitcoincore-rpc` crate
* Increase default `index_lookup_limit` to 200
* Implement 'blockchain.scripthash.listunspent' RPC (#475)
* Update RocksDB to 6.11.4 (#473)
* Allow logging configuration via `RUST_LOG` environment variable (using `env_logger`)

# 0.8.12 (14 Sep 2021)

* Fail if `cookie` is specified (#478)
* Move usage in README up and recommend our guide (#463)
* A bunch of improvements to issue templates (#462)

# 0.8.11 (18 Aug 2021)

* Update dependencies (#401, #402)
* Add python example in RPC examples (#415, #417)
* Add regtest and signet networks in examples (#425)
* Clippy fixes (#430)
* CI fixes (#437, #438, #441)
* Update relevant versions (#450)
* Drop unused compression algorithms for RocksDB

# 0.8.10 (14 May 2021)

* Fix JSONRPC errors' handling (#398, #390)
* Optimize Dockerfile (#387, #388, #392)
* Fix signet default port (https://github.com/romanz/electrs/b53178c140e575b0527a70ead566d50c7fe6cb1f)

# 0.8.9 (19 Mar 2021)

* Use non-batched RPC to reduce bitcoind memory usage (#373)
* Fix inverted logic of deprecation (#379)
* Ignore individual mempool transaction fetch fails (#381)
* Increase default wait_duration_secs to 10s (#384)

# 0.8.8 (22 Feb 2021)

* Deprecate `--cookie` configuration (@Kixunil)
* Update dependencies (@Kixunil)
* Improve documentation (@Kixunil)

# 0.8.7 (15 Jan 2021)

* Support signet (#239)

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
