# Index Schema

The index is stored at a single RocksDB database using the following column families:

## Transaction outputs' index (`funding`)

Allows efficiently finding all funding transactions for a specific address:

|  Script Hash Prefix  | Confirmed Block Height |
| -------------------- | ---------------------- |
| `SHA256(script)[:8]` | `height as u32`        |

## Transaction inputs' index (`spending`)

Allows efficiently finding spending transaction of a specific output:

| Previous Outpoint Prefix | Confirmed Block Height |
| ------------------------ | ---------------------- |
| `txid[:8] as u64 + vout` | `height as u32`        |


## Transaction ID index (`txid`)

In order to save storage space, we map the 8-byte transaction ID prefix to its confirmed block height:

| Txid Prefix | Confirmed height |
| ----------- | ---------------- |
| `txid[:8]`  | `height as u32`  |

Note that this mapping allows us to use `getrawtransaction` RPC to retrieve actual transaction data from without `-txindex` enabled
(by explicitly specifying the [blockhash](https://github.com/bitcoin/bitcoin/commit/497d0e014cc79d46531d570e74e4aeae72db602d)).
