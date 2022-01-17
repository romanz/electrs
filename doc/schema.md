# Index Schema

The index is stored at a single RocksDB database using the following column families.
Most of the data is stored in key-only DB rows (i.e. having empty values).

Note that building this index requires `getblocklocation` RPC (see [commit](https://github.com/romanz/bitcoin/commit/9dd68c4dc1139edc65772c37171053cf8b05ec97)).

## Transaction outputs' index (`funding`)

Allows efficiently finding all funding transactions for a specific address:

|  Script Hash Prefix  |       Transaction position       |
| -------------------- | -------------------------------- |
| `SHA256(script)[:8]` | `(file_id, offset) as (u16, u32)`|

## Transaction inputs' index (`spending`)

Allows efficiently finding spending transaction of a specific output:

| Previous Outpoint Prefix |       Transaction position       |
| ------------------------ | -------------------------------- |
| `txid[:8] as u64 + vout` | `(file_id, offset) as (u16, u32)`|


## Transaction ID index (`txid`)

In order to save storage space, we map the 8-byte transaction ID prefix to its position:

| Txid Prefix |       Transaction position       |
| ----------- | -------------------------------- |
| `txid[:8]`  | `(file_id, offset) as (u16, u32)`|

## Headers (`headers`)

For faster loading, we store all block headers in RocksDB:

|    Serialized header    | Block hash  |           Block position         | Block size |
| ----------------------- | ----------- | -------------------------------- | ---------- |
|      `BlockHeader`      | `BlockHash` | `(file_id, offset) as (u16, u32)`|   `u32`    |

In addition, we also store the chain tip:

| Key |   |           Value          |
| --- | - | ------------------------ |
| `T` |   | `blockhash as BlockHash` |

## Configuration (`config`)

| Key |   |            Value            |
| --- | - | --------------------------- |
| `C` |   | `serialized config as JSON` |

