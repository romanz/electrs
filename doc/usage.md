## Quickstart

Assuming Bitcoin Core 31+ is installed on the same machine (with the standard configuration at `~/.bitcoin/bitcoin.conf`):

```bash
$ bitcoind -server=1 -rest=1 -prune=0 &
$ # ... wait until the chain is synced (e.g. using `bitcoin-cli getblockchaininfo`)
$ RUST_LOG=INFO electrs  --network bitcoin --db-dir ./db --daemon-dir ~/.bitcoin
```

## Usage

First index sync should take ~2 hours for ~800GB `.bitcoin/blocks/` @ July 2026 (on a 6-core CPU, 32 GB RAM, 2TB NVMe):
```bash
$ du -ch ~/.bitcoin/blocks/*.dat | tail -n1
802G  total

$ RUST_LOG=INFO electrs --network bitcoin --db-dir ./db --daemon-dir ~/.bitcoin
Starting electrs 0.12.0 on x86_64 linux with Config { network: Bitcoin, db_dir: "./db", ... }
[2026-07-18T16:37:24.024Z INFO  electrs::metrics::metrics_impl] serving Prometheus metrics on 127.0.0.1:4224
[2026-07-18T16:37:24.025Z INFO  electrs::server] serving Electrum RPC on 127.0.0.1:50001
[2026-07-18T16:37:24.025Z INFO  bindex::chain] index: Config { db_path: "./db/bitcoin", url: "http://127.0.0.1:8332" }
[2026-07-18T16:37:24.054Z INFO  bindex::db] CF config: 0 files, 0.000000 MBs
[2026-07-18T16:37:24.054Z INFO  bindex::db] CF headers: 0 files, 0.000000 MBs
[2026-07-18T16:37:24.054Z INFO  bindex::db] CF txpos: 0 files, 0.000000 MBs
[2026-07-18T16:37:24.054Z INFO  bindex::db] CF txid: 0 files, 0.000000 MBs
[2026-07-18T16:37:24.054Z INFO  bindex::db] CF script_hash: 0 files, 0.000000 MBs
[2026-07-18T16:37:24.054Z INFO  bindex::db] CF spending: 0 files, 0.000000 MBs
[2026-07-18T16:37:24.054Z INFO  bindex::db] CF funding: 0 files, 0.000000 MBs
[2026-07-18T16:37:24.129Z INFO  bindex::chain] block=00000000c937983704a73af28acdec37b049d214adbda81d7e2a3dd146f6ed09 height=1000: indexed 1001 blocks, 0.226[MB], dt = 0.047[s]: 0.047 [ms/block], 0.000 [MB/block], 4.837 [MB/s]
[2026-07-18T16:37:24.196Z INFO  bindex::chain] block=00000000dfd5d65c9d8561b4b8f60a63018fe3933ecb131fb37f905f87da951a height=2000: indexed 1000 blocks, 0.234[MB], dt = 0.043[s]: 0.043 [ms/block], 0.000 [MB/block], 5.376 [MB/s]
[2026-07-18T16:37:24.263Z INFO  bindex::chain] block=000000004a81b9aa469b11649996ecb0a452c16d1181e72f9f980850a1c5ecce height=3000: indexed 1000 blocks, 0.230[MB], dt = 0.043[s]: 0.043 [ms/block], 0.000 [MB/block], 5.298 [MB/s]
[2026-07-18T16:37:24.331Z INFO  bindex::chain] block=00000000922e2aa9e84a474350a3555f49f06061fd49df50a9352f156692a842 height=4000: indexed 1000 blocks, 0.234[MB], dt = 0.044[s]: 0.044 [ms/block], 0.000 [MB/block], 5.366 [MB/s]
[2026-07-18T16:37:24.398Z INFO  bindex::chain] block=000000004d78d2a8a93a1d20a24d721268690bebd2b51f7e80657d57e226eef9 height=5000: indexed 1000 blocks, 0.229[MB], dt = 0.043[s]: 0.043 [ms/block], 0.000 [MB/block], 5.349 [MB/s]
...
[2026-07-18T18:02:21.114Z INFO  bindex::chain] block=0000000000000000000055222909c19ed96cd371013861337214a3c39a63d828 height=955000: indexed 1000 blocks, 1842.985[MB], dt = 9.457[s]: 9.457 [ms/block], 1.843 [MB/block], 194.886 [MB/s]
[2026-07-18T18:02:33.592Z INFO  bindex::chain] block=000000000000000000012d9ea47c8b3282ae1a7792d54b8b4bcc0536c1883191 height=956000: indexed 1000 blocks, 1846.173[MB], dt = 9.522[s]: 9.522 [ms/block], 1.846 [MB/block], 193.879 [MB/s]
[2026-07-18T18:02:45.872Z INFO  bindex::chain] block=00000000000000000000e898fc9d01b0729d8830f2068248debbff575a3b0990 height=957000: indexed 1000 blocks, 1855.303[MB], dt = 9.295[s]: 9.295 [ms/block], 1.855 [MB/block], 199.600 [MB/s]
[2026-07-18T18:02:58.221Z INFO  bindex::chain] block=000000000000000000022c6a6d538a4372b0eeadb4ea9eb2edd4e32ec780084b height=958000: indexed 1000 blocks, 1844.747[MB], dt = 9.376[s]: 9.376 [ms/block], 1.845 [MB/block], 196.759 [MB/s]
[2026-07-18T18:03:05.539Z INFO  bindex::chain] block=000000000000000000020934afa0945d959250e9a4598d39c53fa280639b096a height=958594: indexed 594 blocks, 1115.055[MB], dt = 5.357[s]: 9.018 [ms/block], 1.877 [MB/block], 208.155 [MB/s]
[2026-07-18T18:03:05.666Z INFO  bindex::db] started auto compactions
```
You can specify options via command-line parameters, environment variables or using config files.
See the documentation above.

Note that the final DB size should be ~7% of the `blocks/*.dat` files, but it may increase to ~14% at the end of the initial sync (just before the [full compaction is invoked](https://github.com/facebook/rocksdb/wiki/Manual-Compaction)).

It should take roughly 18 hours to sync and compact the index on an ODROID-HC1 with 8 CPU cores @ 2GHz, 2GB RAM, and an SSD using the command above.

The index database is stored here:
```bash
$ du -h db/bitcoin/
56G	db/bitcoin/
```

See [extra configuration suggestions](config.md#extra-configuration-suggestions) that you might want to consider.

## Electrum client

If you happen to use the Electrum client from [the *beta* Debian repository](binaries.md#native-os-packages), it's pre-configured out-of-the-box already
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
{"id":0,"jsonrpc":"2.0","result":["electrs 0.12.0","1.4"]}
```

For more complex tasks, you may need to convert addresses to
[script hashes](https://electrum-protocol.readthedocs.io/en/latest/protocol-basics.html#script-hashes) - see
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
