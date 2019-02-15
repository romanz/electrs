# Esplora - Electrs backend API

A block chain index engine and HTTP API written in Rust based on [romanz/electrs](https://github.com/romanz/electrs).

Used as the backend for the [Esplora block explorer](https://github.com/Blockstream/esplora) powering [blockstream.info](https://blockstream.info/).

API documentation [is available here](https://github.com/blockstream/esplora/blob/master/API.md).

Documentation for the database schema and indexing process [is available here](doc/schema.md).

### Installing & indexing

Install Rust, Bitcoin Core and the `clang` and `cmake` packages, then:

```bash
$ git clone https://github.com/blockstream/electrs && cd electrs
$ git checkout new-index
$ cargo run --release --bin electrs -- -vvvv --daemon-dir ~/.bitcoin

# Or for liquid:
$ cargo run --features liquid --release --bin electrs -- -vvvv --daemon-dir ~/.liquid
```

See [electrs's original documentation](https://github.com/romanz/electrs/blob/master/doc/usage.md) for more detailed instructions.
Note that our indexes are incompatible with electrs's and has to be created separately.

The indexes require 440GB of storage after running compaction, but you'll need to have about 1TB
of free space available for the initial (non-compacted) indexing process.
Creating the indexes should take a few hours on a beefy machine with SSD.

> Note: the pre-v2 version supported a "light" indexing mode for personal use,
> where less data is kept on-disk in exchange for a performance hit and more reliance/load on bitcoind.
> This option is not currently available, but can be enabled with [an older esplora release](https://github.com/Blockstream/esplora/releases/tag/esplora_v1.67).
> We're hoping to eventually get this feature back. Let us know if you find this important!

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
  (requires enabling the `liquid` feature flag using `--features liquid`)

### CLI options

In addition to electrs's original configuration options, a few new options are also available:

- `--http-addr <addr:port>` - HTTP server address/port to listen on (default: `127.0.0.1:3000`).
- `--disable-prevout` - disable attaching previous output information to inputs.
  This significantly reduces the amount of transaction lookups (and IO/CPU/memory usage),
  at the cost of not knowing inputs amounts, their previous script/address, and the transaction fee.
- `--parent-network <network>` - the parent network this chain is pegged to (Elements/Liquid only).
- `--cors <origins>` - origins allowed to make cross-site request (optional, defaults to none).

See `$ cargo run --release --bin electrs -- --help` for the full list of options.

## License

MIT
