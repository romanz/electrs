Electrum
========
* Poll mempool after transaction broadcase
* Update subscriptions after index/mempool update
* Snapshot DB after successful indexing - and run queries on the latest snapshot
* Update height to -1 for txns with any `unconfirmed input <https://electrumx.readthedocs.io/en/latest/protocol-basics.html#status>`_

Bitcoind
========
* Use persistent connection for donwloading multiple blocks
* Use p2p protocol for querying blocks - similar to `bitcoincore-indexd`
* Handle bitcoind connection failures - instead of crashing
* Add getrawtransactions() API (for RPC batching)

Performance
===========
* Export accumulated timing metrics (for indexing/DB/RPC operations)
* Measure first-time query latency
* Sync only on the last write.

Rust
====
* Use Bytes instead of Vec[u8] when possible
* Return errors instead of panics
* Use generators instead of vectors
