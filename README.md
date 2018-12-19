# Esplora - Electrs backend API

A block chain index engine and HTTP API written in Rust based on [romanz/electrs](https://github.com/romanz/electrs).

Used as the backend for the [Esplora block explorer](https://github.com/Blockstream/esplora) powering [blockstream.info](https://blockstream.info/).

API documentation [is available here](https://github.com/blockstream/esplora/blob/master/API.md).

### Installing & indexing

Install Rust, Bitcoin Core and the `clang` and `cmake` packages, then:

```bash
$ git clone https://github.com/blockstream/electrs && cd electrs
$ git checkout bitcoin_e # or liquid_e
$ cargo run --release -- -vvvv --daemon-dir ~/.bitcoin
```

See [electrs's original documentation](https://github.com/romanz/electrs/blob/master/doc/usage.md) for more detailed instructions.
Note that our indexes are incompatible with electrs's and has to be created separately.

The indexes require 250GB-300GB of storage after running compaction, but you'll need to have at least 500GB
of free space available for the initial (non-compacted) indexing process.
Creating the indexes should take a few hours on a beefy machine with SSD.

For personal low-volume use, the storage requirements can be reduced with `--light` (see below under CLI options).

To deploy with Docker, follow the [instructions here](https://github.com/Blockstream/esplora#how-to-build-the-docker-image).

### Notable changes from Electrs:

- HTTP REST API instead of the Electrum JSON-RPC protocol, with extended transaction information
  (previous outputs, spending transactions, script asm and more).

- Extended indexes and database storage for improved performance under high load:

  - A full transaction store mapping txids to raw transactions is kept in the database under the prefix `t`.
    This takes up ~200GB of extra storage.
  - A map of blockhash to txids is kept in the database under the prefix `X`.
  - Block stats metadata (number of transactions, size and weight) is kept in the database under the prefix `M`.
  - The index with `T` prefix mapping txids to block heights now also includes the block hash.
    This allows for quick reorg-aware transaction confirmation status lookups, by verifying the
    current block at the recorded height still matches the recorded block hash.

  With these new indexes, bitcoind is no longer queried to serve user requests and is only polled
  periodically for new blocks and for syncing the mempool.

- Support for Liquid and other Elements-based networks, including CT, peg-in/out and multi-asset.
  (under the `liquid_e` branch)

### CLI options

In addition to electrs's original configuration options, a few new options are also available:

- `--http-addr <addr:port>` - HTTP server address/port to listen on (default: `127.0.0.1:3000`).
- `--light` - enable light resource mode, which disables the `X`, `M` and `t` indexes
   and queries this information from bitcoind instead.
   This significantly reduces storage requirements (at the time of writing, by about 250GB),
   at the cost of more expensive lookups and more reliance on bitcoind.
- `--disable-prevout` - disable attaching previous output information to inputs.
  This significantly reduces the amount of transaction lookups (and IO/CPU/memory usage),
  at the cost of not knowing inputs amounts, their previous script/address, and the transaction fee.
  Consider setting this if you're using `--light`.
- `--parent-network <network>` - the parent network this chain is pegged to (Elements/Liquid only).

See `$ cargo run --release -- --help` for the full list of options.

## License

MIT
