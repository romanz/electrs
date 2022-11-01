### Important changes from versions older than 0.9.3

* If you use `verbose` (or `-v` argument), switch to `log_filters` (or `RUST_LOG` environment variable).
  Please note that it allows to set per-module filters, but module naming is considered unstable.
  If you have used `-vv` (the value suggested in the documentation), switch to `--log-filters INFO`:


|Log filter|Old `verbose` value|Description                                                           |
|----------|-------------------|----------------------------------------------------------------------|
|ERROR     |                  0|Only fatal errors                                                     |
|WARN      |                  1|Things that could indicate serious problems                           |
|INFO      |                  2|Various significant events and suggestions                            |
|DEBUG     |                  3|Details that could be useful when debugging - only use when debugging!|
|TRACE     |                  4|**Very** detailed information - only use when debugging!              |  


### Important changes from versions older than 0.9.0

In 0.9.0 we have changed the RocksDB index format to optimize electrs performance.
We also use Bitcoin P2P protocol instead of reading blocks from disk or JSON RPC.
Some guides were suggesting trace log level and we started to trace much more information.

Upgrading checklist:

* Make sure you upgrade at time when you don't need to use electrs for a while.
  Because of reindex electrs will be unable to serve your requests for a few hours.
  (The exact time depends on your hardware.)
  If you wish to check the database without reindexing run electrs with `--no-auto-reindex`.
* If you have less than 60 GB of free space delete `mainnet` subdirectory inside your `db_dir` *before* running the new version.
  Note however if you have less than 60 GB of free space you should consider extending your storage soon
  since in the worst case scenario you will run out of space in ~100 days.
* Make sure to allow accesses to bitcoind from local address, ideally whitelist it using `whitelist=download@127.0.0.1` bitcoind option.
  Either don't use `maxconnections` bitcoind option or set it to 12 or more.
* If you use non-default P2P port (or address) for bitcoind adjust `electrs` configuration.
* If you still didn't migrate `cookie` electrs option you have to now - see below.
* Remove unsupported options from configuration (`blocks_dir`, `jsonrpc_import`, `bulk_index_threads`, `tx_cache_size_mb`, `blocktxids_cache_size_mb`)
* Rename `txid_limit` to `index_lookup_limit` if used
* If you use `verbose = 4` (or `-vvvv` argument) lower it down to `2` (`-vv`) for production use.
  Keeping it would waste resources because we utilize it more now.
* **After reindexing**, if you did **not** delete `mainnet` subdirectory within `db_dir` check that `electrs` works as expected and then *delete whole `mainnet` subdirectory*.
* If you are using our Dockerfile, please make sure to re-map the DB volume (see [the section above](docker.md#docker-based-installation-from-source)).

### Important changes from version older than 0.8.8

**If you're upgrading from version 0.8.7 to a higher version and used `cookie` option you should change your configuration!**
The `cookie` option was deprecated and **will be removed eventually**!
If you had actual cookie (from `~/bitcoin/.cookie` file) specified in `cookie` option, this was wrong as it wouldn't get updated when needed.
It's strongly recommended to use proper cookie authentication using `cookie_file`.
If you really have to use fixed username and password, explicitly specified in `bitcoind` config, use `auth` option instead.
Users of `btc-rpc-proxy` using `public:public` need to use `auth` too.
You can read [a detailed explanation of cookie deprecation with motivation explained](cookie_deprecation.md).

### General upgrading guide

As with any other application, you need to remember how you installed `electrs` to upgrade it.
If you don't then here's a little help: run `which electrs` and compare the output

* If you got an error you didn't install `electrs` into your system in any way, it's probably sitting in the `target/release` directory of source
* If the path starts with `/bin/` then either you have used packaging system or you made a mistake the first time (non-packaged binaries must go to `/usr/local/bin`)
* If the path starts with `/usr/local/bin` you most likely copied electrs there after building
* If the path starts with `/home/YOUR_USERNAME/.cargo/bin` you most likely ran `cargo install`

### Upgrading distribution package

If you used Debian packaging system you only need this:

```
sudo apt update
sudo apt upgrade
```

Similarly for other distributions - use their respective commands.  
If a new version of `electrs` is not yet in the package system, try wait a few days or contact the maintainers of the packages if it's been a long time.

### Upgrading manual installation

1. Enter your `electrs` source directory, usually in `~/` but some people like to put it in something like `~/sources`.
   If you've deleted it, you need to `git clone` again.
2. `git checkout master`
3. `git pull`
4. Strongly recommended: `git verify-tag v0.9.1` (fix the version number if we've forgotten to update the docs ;)) should show "Good signature from 15C8 C357 4AE4 F1E2 5F3F 35C5 87CA E5FA 4691 7CBB"
5. `git checkout v0.9.1`
6. If you used static linking: `cargo build --locked --release`.
   If you used dynamic linking `ROCKSDB_INCLUDE_DIR=/usr/include ROCKSDB_LIB_DIR=/usr/lib cargo build --locked --release`.
   If you don't remember which linking you used, you probably used static.
   This step will take a few tens of minutes (but dynamic linking is a bit faster), go grab a coffee.
   Also remember that you need enough free RAM, the build will die otherwise
7. If you've previously copied `electrs` into `/usr/local/bin` run: sudo `cp target/release/electrs /usr/local/bin`
   If you've previously installed `electrs` using `cargo install`: `cargo install --locked --path . -f`
8. If you've manually configured systemd service: `sudo systemctl restart electrs`
