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

First index sync should take ~6.5 hours for ~504GB @ August 2023 (on a dual core Intel CPU @ 3.3 GHz, 8 GB RAM, 1TB WD Blue HDD):
```bash
$ du -ch ~/.bitcoin/blocks/blk*.dat | tail -n1
336G  total

$ ./target/release/electrs --network bitcoin --db-dir ./db --daemon-dir /home/user/.bitcoin
Starting electrs 0.10.0 on x86_64 linux with Config { network: Bitcoin, db_path: "./db/bitcoin", daemon_dir: "/home/user/.bitcoin", daemon_auth: CookieFile("/home/user/.bitcoin/.cookie"), daemon_rpc_addr: 127.0.0.1:8332, daemon_p2p_addr: 127.0.0.1:8333, electrum_rpc_addr: 127.0.0.1:50001, monitoring_addr: 127.0.0.1:4224, wait_duration: 10s, jsonrpc_timeout: 15s, index_batch_size: 10, index_lookup_limit: None, reindex_last_blocks: 0, auto_reindex: true, ignore_mempool: false, sync_once: false, skip_block_download_wait: false, disable_electrum_rpc: false, server_banner: "Welcome to electrs 0.10.0 (Electrum Rust Server)!", signet_magic: f9beb4d9, args: [] }
[2023-08-16T19:17:11.193Z INFO  electrs::metrics::metrics_impl] serving Prometheus metrics on 127.0.0.1:4224
[2023-08-16T19:17:11.193Z INFO  electrs::server] serving Electrum RPC on 127.0.0.1:50001
[2023-08-16T19:17:12.355Z INFO  electrs::db] "./db/bitcoin": 0 SST files, 0 GB, 0 Grows
[2023-08-16T19:17:12.446Z INFO  electrs::index] indexing 2000 blocks: [1..2000]
[2023-08-16T19:17:12.866Z INFO  electrs::chain] chain updated: tip=00000000dfd5d65c9d8561b4b8f60a63018fe3933ecb131fb37f905f87da951a, height=2000
[2023-08-16T19:17:12.879Z INFO  electrs::index] indexing 2000 blocks: [2001..4000]
[2023-08-16T19:17:13.227Z INFO  electrs::chain] chain updated: tip=00000000922e2aa9e84a474350a3555f49f06061fd49df50a9352f156692a842, height=4000
[2023-08-16T19:17:13.238Z INFO  electrs::index] indexing 2000 blocks: [4001..6000]
[2023-08-16T19:17:13.587Z INFO  electrs::chain] chain updated: tip=00000000dbbb79792303bdd1c6c4d7ab9c21bba0667213c2eca955e11230c5a5, height=6000
[2023-08-16T19:17:13.598Z INFO  electrs::index] indexing 2000 blocks: [6001..8000]
[2023-08-16T19:17:13.950Z INFO  electrs::chain] chain updated: tip=0000000094fbacdffec05aea9847000522a258c269ae37a74a818afb96fc27d9, height=8000
[2023-08-16T19:17:13.961Z INFO  electrs::index] indexing 2000 blocks: [8001..10000]
<...>
[2023-08-17T00:13:16.443Z INFO  electrs::index] indexing 2000 blocks: [798001..800000]
[2023-08-17T00:14:58.310Z INFO  electrs::chain] chain updated: tip=00000000000000000002a7c4c1e48d76c5a37902165a270156b7a8d72728a054, height=800000
[2023-08-17T00:14:58.325Z INFO  electrs::index] indexing 2000 blocks: [800001..802000]
[2023-08-17T00:16:36.425Z INFO  electrs::chain] chain updated: tip=0000000000000000000311b41f1d611f977b024b947568c1dd760704360f148a, height=802000
[2023-08-17T00:16:36.437Z INFO  electrs::index] indexing 1534 blocks: [802001..803534]
[2023-08-17T00:17:51.338Z INFO  electrs::chain] chain updated: tip=00000000000000000003c0cd1b62ed8bb502e24bcbfeee16e81d6ea33d026263, height=803534
[2023-08-17T00:18:00.592Z INFO  electrs::db] starting config compaction
[2023-08-17T00:18:00.778Z INFO  electrs::db] starting headers compaction
[2023-08-17T00:18:00.870Z INFO  electrs::db] starting txid compaction
[2023-08-17T00:33:34.370Z INFO  electrs::db] starting funding compaction
[2023-08-17T01:03:56.784Z INFO  electrs::db] starting spending compaction
[2023-08-17T01:35:05.983Z INFO  electrs::db] finished full compaction
[2023-08-17T01:36:23.300Z INFO  electrs::index] indexing 5 blocks: [803535..803539]
[2023-08-17T01:36:23.646Z INFO  electrs::chain] chain updated: tip=000000000000000000006a3aaddd4b643607b33e000f1200d35005c330ecfa88, height=803539
[2023-08-17T01:41:26.009Z INFO  electrs::index] indexing 1 blocks: [803540..803540]
[2023-08-17T01:41:26.143Z INFO  electrs::chain] chain updated: tip=00000000000000000003266d31db92629b64241eef7ce708244f6d6283b080b4, height=803540
[2023-08-17T01:42:42.999Z INFO  electrs::index] indexing 1 blocks: [803541..803541]
[2023-08-17T01:42:43.153Z INFO  electrs::chain] chain updated: tip=00000000000000000000884a77c8b8ad2fb0c25510a3251bf5ef57f0db275146, height=803541
```
You can specify options via command-line parameters, environment variables or using config files.
See the documentation above.

Note that the final DB size should be ~10% of the `blk*.dat` files, but it may increase to ~20% at the end of the initial sync (just before the [full compaction is invoked](https://github.com/facebook/rocksdb/wiki/Manual-Compaction)).

It should take roughly 18 hours to sync and compact the index on an ODROID-HC1 with 8 CPU cores @ 2GHz, 2GB RAM, and an SSD using the command above.

The index database is stored here:
```bash
$ du db/
42G db/mainnet/
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
