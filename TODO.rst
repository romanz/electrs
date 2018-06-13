Electrum
========
* Gracefully stop RPC server after SIGINT
* Poll mempool after transaction broadcast
* Snapshot DB after successful indexing - and run queries on the latest snapshot
* Update height to -1 for txns with any `unconfirmed input <https://electrumx.readthedocs.io/en/latest/protocol-basics.html#status>`_

Bitcoind
========
* Handle bitcoind connection failures - instead of crashing
* Add getrawtransactions() API (for RPC batching)

Performance
===========
* Experiment with `sled <https://github.com/spacejam/sled>`_ DB

Rust
====
* Use Bytes instead of Vec[u8] when possible
* Return errors instead of panics
* Use generators instead of vectors
