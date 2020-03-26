# Index Schema

The index is stored as three RocksDB databases:

- `txstore`
- `history`
- `cache`

### Indexing process

The indexing is done in the two phase, where each can be done concurrently within itself.
The first phase populates the `txstore` database, the second phase populates the `history` database.

NOTE: in order to construct the history rows for spending inputs in phase #2, we rely on having the transactions being processed at phase #1, so they can be looked up efficiently (using parallel point lookups).

After the indexing is completed, both funding and spending are indexed as independent rows under `H{scripthash}`, so that they can be queried in-order in one go.

### `txstore`

Each block results in the following new rows:

 * `"B{blockhash}" → "{header}"`

 * `"X{blockhash}" → "{txids}"` (list of txids included in the block)

 * `"M{blockhash}" → "{metadata}"` (block weight, size and number of txs)

 * `"D{blockhash}" → ""` (signifies the block is done processing)

Each transaction results in the following new rows:

 * `"T{txid}" → "{serialized-transaction}"`

 * `"C{txid}{confirmed-blockhash}" → ""` (a list of blockhashes where `txid` was seen to be confirmed)

Each output results in the following new row:

 * `"O{txid}{vout}" → "{scriptpubkey}{value}"`

When the indexer is synced up to the tip of the chain, the hash of the tip is saved as following:

 * `"t" →  "{blockhash}"`

### `history`

Each funding output (except for provably unspendable ones) results in the following new row (`H` is for history, `F` is for funding):

 * `"H{funding-scripthash}{funding-height}F{funding-txid:vout}{value}" → ""`
 * `"a{funding-address}" → ""` (for prefix address search, only saved when `--address-search` is enabled)

Each spending input (except the coinbase) results in the following new rows (`S` is for spending):

 * `"H{funding-scripthash}{spending-height}S{spending-txid:vin}{funding-txid:vout}{value}" → ""`

 * `"S{funding-txid:vout}{spending-txid:vin}" → ""`

Liquid/elements chains also have the following indexes for issued assets:

 * `"i{asset-id}" → "{issuing-txid:vin}{prev-txid:vout}{issuance}{reissuance_token}"`
 * `"I{asset-id}{issuance-height}I{issuing-txid:vin}{is_reissuance}{amount}{tokens}" → ""`
 * `"I{asset-id}{funding-height}F{funding-txid:vout}{value}" → ""`
 * `"I{asset-id}{spending-height}S{spending-txid:vin}{funding-txid:vout}{value}" → ""`

### `cache`

Holds a cache for aggregated stats and unspent TXOs of scripthashes.

The cache is created on-demand, the first time the scripthash is requested by a user.

The cached data is kept next to the `blockhash` the cache is up-to-date for.
When requesting data, the cache is updated with the new history rows added since the `blockhash`.
If the `blockhash` was since orphaned, the cache is removed and re-computed.

 * `"A{scripthash}" → "{stats}{blockhash}"` (where `stats` is composed of `tx_count`, `funded_txo_{count,sum}` and `spent_txo_{count,sum}`)

 * `"U{scripthash}" → "{utxo}{blockhash}"` (where `utxo` is a set of `(txid,vout)` outpoints)

Elements only:

 * `"z{asset-id}" → "{stats}{blockhash}"` (where `stats` is composed of `tx_count`, `issuance_count`, `issued_amount`, `burned_amount`, `has_blinded_issuances`, `reissuance_tokens`, `burned_reissuance_tokens`)
