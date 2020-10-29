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

Each funding output (except for provably unspendable ones when `--index-unspendables` is not enabled) results in the following new rows (`H` is for history, `F` is for funding):

 * `"H{funding-scripthash}{funding-height}F{funding-txid:vout}{value}" → ""`
 * `"a{funding-address-str}" → ""` (for prefix address search, only saved when `--address-search` is enabled)

Each spending input (except the coinbase) results in the following new rows (`S` is for spending):

 * `"H{funding-scripthash}{spending-height}S{spending-txid:vin}{funding-txid:vout}{value}" → ""`

 * `"S{funding-txid:vout}{spending-txid:vin}" → ""`

#### Elements only

Assets (re)issuances results in the following new rows (only for user-issued assets):

 * `"i{asset-id}" → "{issuing-txid:vin}{prev-txid:vout}{issuance}{reissuance_token}"`
 * `"I{asset-id}{issuance-height}I{issuing-txid:vin}{is_reissuance}{amount}{tokens}" → ""`

Peg-ins/peg-outs results in the following new rows (only for the native asset, typically L-BTC):

 * `"I{asset-id}{pegin-height}F{pegin-txid:vin}{value}" → ""`
 * `"I{asset-id}{pegout-height}F{pegout-txid:vout}{value}" → ""`

Every burn (unspendable output) results in the following new row (both user-issued and native):

 * `"I{asset-id}{burn-height}F{burning-txid:vout}{value}" → ""`

### `cache`

Holds a cache for aggregated stats and unspent TXOs of scripthashes.

The cache is created on-demand, the first time the scripthash is requested by a user.

The cached data is kept next to the `blockhash` the cache is up-to-date for.
When requesting data, the cache is updated with the new history rows added since the `blockhash`.
If the `blockhash` was since orphaned, the cache is removed and re-computed.

 * `"A{scripthash}" → "{stats}{blockhash}"` (where `stats` is composed of `tx_count`, `funded_txo_{count,sum}` and `spent_txo_{count,sum}`)

 * `"U{scripthash}" → "{utxo}{blockhash}"` (where `utxo` is a set of `(txid,vout)` outpoints)

#### Elements only:

Stats for issued assets:
 * `"z{asset-id}" → "{issued_stats}{blockhash}"` (where `issued_stats` is composed of `tx_count`, `issuance_count`, `issued_amount`, `burned_amount`, `has_blinded_issuances`, `reissuance_tokens`, `burned_reissuance_tokens`)

Stats for the native asset:
 * `"z{issued-asset}" → "{native_stats}{blockhash}"` (where `native_stats` is composed of `tx_count`, `peg_in_count`, `peg_in_amount`, `peg_out_count`, `peg_out_amount`, `burn_count` and `burn_amount`)
