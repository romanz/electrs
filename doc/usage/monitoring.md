## Monitoring

Indexing and serving metrics are exported via [Prometheus](https://github.com/pingcap/rust-prometheus):

```bash
$ sudo apt install prometheus
$ echo "
scrape_configs:
  - job_name: electrs
    static_configs:
      - targets: ['localhost:4224']
" | sudo tee -a /etc/prometheus/prometheus.yml
$ sudo systemctl restart prometheus
$ firefox 'http://localhost:9090/graph?g0.range_input=1h&g0.expr=index_height&g0.tab=0'
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