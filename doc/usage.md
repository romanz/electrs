## Quickstart

<details>
<summary>Assuming Bitcoin Core 0.21+ is installed on the same machine (with the standard configuration at `~/.bitcoin/bitcoin.conf`):</summary>

```bash
$ bitcoind -server=1 -prune=0 &
$ # ... wait until the chain is synced (e.g. using `bitcoin-cli getblockchaininfo`)
$ electrs --log-filters=INFO --db-dir ./db --daemon-dir ~/.bitcoin --network bitcoin
```

</details>

[![asciicast](https://asciinema.org/a/zRNZp5HsBDi5rAlGWU7470Pzl.svg)](https://asciinema.org/a/zRNZp5HsBDi5rAlGWU7470Pzl?speed=3)

## Usage

First index sync should take ~4 hours for ~336GB @ August 2021 (on a dual core Intel CPU @ 3.3 GHz, 8 GB RAM, 1TB WD Blue HDD):
```bash
$ du -ch ~/.bitcoin/blocks/blk*.dat | tail -n1
336G  total

$ ./target/release/electrs --log-filters INFO --db-dir ./db --electrum-rpc-addr="127.0.0.1:50001"
Config { network: Bitcoin, db_path: "./db/bitcoin", daemon_dir: "/home/user/.bitcoin", daemon_auth: CookieFile("/home/user/.bitcoin/.cookie"), daemon_rpc_addr: V4(127.0.0.1:8332), daemon_p2p_addr: V4(127.0.0.1:8333), electrum_rpc_addr: V4(127.0.0.1:50001), monitoring_addr: V4(127.0.0.1:4224), wait_duration: 10s, index_batch_size: 10, index_lookup_limit: 100, ignore_mempool: false, server_banner: "Welcome to electrs 0.9.0 (Electrum Rust Server)!", args: [] }
[2021-08-17T18:48:40.054Z INFO  electrs::metrics::metrics_impl] serving Prometheus metrics on 127.0.0.1:4224
[2021-08-17T18:48:40.944Z INFO  electrs::db] "./db/bitcoin": 0 SST files, 0 GB, 0 Grows
[2021-08-17T18:48:41.075Z INFO  electrs::index] indexing 2000 blocks: [1..2000]
[2021-08-17T18:48:41.610Z INFO  electrs::chain] chain updated: tip=00000000dfd5d65c9d8561b4b8f60a63018fe3933ecb131fb37f905f87da951a, height=2000
[2021-08-17T18:48:41.623Z INFO  electrs::index] indexing 2000 blocks: [2001..4000]
[2021-08-17T18:48:42.178Z INFO  electrs::chain] chain updated: tip=00000000922e2aa9e84a474350a3555f49f06061fd49df50a9352f156692a842, height=4000
[2021-08-17T18:48:42.188Z INFO  electrs::index] indexing 2000 blocks: [4001..6000]
[2021-08-17T18:48:42.714Z INFO  electrs::chain] chain updated: tip=00000000dbbb79792303bdd1c6c4d7ab9c21bba0667213c2eca955e11230c5a5, height=6000
[2021-08-17T18:48:42.723Z INFO  electrs::index] indexing 2000 blocks: [6001..8000]
[2021-08-17T18:48:43.235Z INFO  electrs::chain] chain updated: tip=0000000094fbacdffec05aea9847000522a258c269ae37a74a818afb96fc27d9, height=8000
[2021-08-17T18:48:43.246Z INFO  electrs::index] indexing 2000 blocks: [8001..10000]
[2021-08-17T18:48:43.768Z INFO  electrs::chain] chain updated: tip=0000000099c744455f58e6c6e98b671e1bf7f37346bfd4cf5d0274ad8ee660cb, height=10000
<...>
[2021-08-17T22:11:20.139Z INFO  electrs::chain] chain updated: tip=00000000000000000002a23d6df20eecec15b21d32c75833cce28f113de888b7, height=690000
[2021-08-17T22:11:20.157Z INFO  electrs::index] indexing 2000 blocks: [690001..692000]
[2021-08-17T22:12:16.944Z INFO  electrs::chain] chain updated: tip=000000000000000000054dab4b85860fcee5808ab7357eb2bb45114a25b77380, height=692000
[2021-08-17T22:12:16.957Z INFO  electrs::index] indexing 2000 blocks: [692001..694000]
[2021-08-17T22:13:11.764Z INFO  electrs::chain] chain updated: tip=00000000000000000003f5acb5ec81df7c98c16bc8d89bdaadd4e8965729c018, height=694000
[2021-08-17T22:13:11.777Z INFO  electrs::index] indexing 2000 blocks: [694001..696000]
[2021-08-17T22:14:05.852Z INFO  electrs::chain] chain updated: tip=0000000000000000000dfc81671ac5a22d8751f9c1506689d3eaceaef26470b9, height=696000
[2021-08-17T22:14:05.855Z INFO  electrs::index] indexing 295 blocks: [696001..696295]
[2021-08-17T22:14:15.557Z INFO  electrs::chain] chain updated: tip=0000000000000000000eceb67a01c81c65b538a7b3729f879c6c1e248bb6577a, height=696295
[2021-08-17T22:14:21.578Z INFO  electrs::db] starting config compaction
[2021-08-17T22:14:21.623Z INFO  electrs::db] starting headers compaction
[2021-08-17T22:14:21.667Z INFO  electrs::db] starting txid compaction
[2021-08-17T22:22:27.009Z INFO  electrs::db] starting funding compaction
[2021-08-17T22:38:17.104Z INFO  electrs::db] starting spending compaction
[2021-08-17T22:55:11.785Z INFO  electrs::db] finished full compaction
[2021-08-17T22:55:15.835Z INFO  electrs::server] serving Electrum RPC on 127.0.0.1:50001
[2021-08-17T22:55:25.837Z INFO  electrs::index] indexing 7 blocks: [696296..696302]
[2021-08-17T22:55:26.120Z INFO  electrs::chain] chain updated: tip=0000000000000000000059e97dea0b0b9ebf4ac1fd66726b339fe1c9683de656, height=696302
[2021-08-17T23:02:03.453Z INFO  electrs::index] indexing 1 blocks: [696303..696303]
[2021-08-17T23:02:03.691Z INFO  electrs::chain] chain updated: tip=000000000000000000088107c337bf315e2db1e406c50566bd765f04a7e459b6, height=696303
```
You can specify options via command-line parameters, environment variables or using config files.
See the documentation above.

Note that the final DB size should be ~10% of the `blk*.dat` files, but it may increase to ~20% at the end of the initial sync (just before the [full compaction is invoked](https://github.com/facebook/rocksdb/wiki/Manual-Compaction)).

It should take roughly 18 hours to sync and compact the index on an ODROID-HC1 with 8 CPU cores @ 2GHz, 2GB RAM, and an SSD using the command above.

The index database is stored here:
```bash
$ du db/
30G db/mainnet/
```

See [extra configuration suggestions](config.md#extra-configuration-suggestions) that you might want to consider.

## Electrum client

If you happen to use the Electrum client from [the *beta* Debian repository](binaries.md#cnative-os-packages), it's pre-configured out-of-the-box already
Read below otherwise.

There's a prepared script for launching `electrum` in such way to connect only to the local `electrs` instance to protect your privacy.

```bash
$ ./contrib/local-electrum.bash
+ ADDR=127.0.0.1
+ PORT=50001
+ PROTOCOL=t
+ electrum --oneserver --server=127.0.0.1:50001:t
<snip>
```

You can persist Electrum configuration (see `~/.electrum/config`) using:
```bash
$ electrum setconfig oneserver true
$ electrum setconfig server 127.0.0.1:50001:t
$ electrum   # will connect only to the local server
```

## RPC examples

You can invoke any supported RPC using `netcat`, for example:

```
$ echo '{"jsonrpc": "2.0", "method": "server.version", "params": ["", "1.4"], "id": 0}' | netcat 127.0.0.1 50001
{"id":0,"jsonrpc":"2.0","result":["electrs 0.9.0","1.4"]}
```

For more complex tasks, you may need to convert addresses to 
[script hashes](https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-basics.html#script-hashes) - see 
[contrib/history.py](https://github.com/romanz/electrs/blob/master/contrib/history.py) for getting an address balance and history:

```
$ ./contrib/history.sh --venv 144STc7gcb9XCp6t4hvrcUEKg9KemivsCR
[2021-08-18 13:56:40.254317] INFO: electrum: connecting to localhost:50001
[2021-08-18 13:56:40.574461] INFO: electrum: subscribed to 1 scripthashes
[2021-08-18 13:56:40.645072] DEBUG: electrum:         0.00000 mBTC (total)
[2021-08-18 13:56:40.710279] INFO: electrum: got history of 2 transactions
[2021-08-18 13:56:40.769064] INFO: electrum: loaded 2 transactions
[2021-08-18 13:56:40.835569] INFO: electrum: loaded 2 header timestamps
[2021-08-18 13:56:40.900560] INFO: electrum: loaded 2 merkle proofs
+------------------------------------------------------------------+----------------------+--------+---------------+--------------+--------------+
|                               txid                               |   block timestamp    | height | confirmations | delta (mBTC) | total (mBTC) |
+------------------------------------------------------------------+----------------------+--------+---------------+--------------+--------------+
| 34b6411d004f279622d0a45a4558746e1fa74323c5c01e9c0bb0a3277781a0d0 | 2020-07-25T08:33:57Z | 640699 |     55689     |    126.52436 |    126.52436 |
| e58916ca945639c657de137b30bd29e213e4c9fc8e04652c1abc2922909fb8fd | 2020-07-25T21:20:35Z | 640775 |     55613     |   -126.52436 |      0.00000 |
+------------------------------------------------------------------+----------------------+--------+---------------+--------------+--------------+
[2021-08-18 13:56:40.902677] INFO: electrum: tip=00000000000000000009d7590d32ca52ad0b8a4cdfee43e28e6dfcd11cafeaac, height=696387 @ 2021-08-18T13:47:19Z
```
