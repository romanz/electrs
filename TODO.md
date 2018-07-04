# Electrum

* Poll mempool after transaction broadcast
* Support TLS (via https://docs.rs/rustls/)
* Snapshot DB after successful indexing - and run queries on the latest snapshot
* Update height to -1 for txns with any [unconfirmed input](https://electrumx.readthedocs.io/en/latest/protocol-basics.html#status)
* Limit mempool TXs (e.g. by fee rate) when mempool is large

# Bitcoind

* Add getrawtransactions() API (for RPC batching)

# Performance

* Experiment with [SSTable ingestion](https://github.com/facebook/rocksdb/wiki/Creating-and-Ingesting-SST-files)
* Use rayon for faster multi-block indexing on multi-core systems

# Rust

* Use [bytes](https://carllerche.github.io/bytes/bytes/index.html) instead of `Vec<u8>` when possible
* Use generators instead of vectors
