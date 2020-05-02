## Manual installation from source

**See below for automated/binary installation options.**

### Build dependencies

Install [recent Rust](https://rustup.rs/) (1.34+, `apt install cargo` is preferred for Debian 10),
[latest Bitcoin Core](https://bitcoincore.org/en/download/) (0.16+)
and [latest Electrum wallet](https://electrum.org/#download) (3.3+).

Also, install the following packages (on Debian):
```bash
$ sudo apt update
$ sudo apt install clang cmake  # for building 'rust-rocksdb'
```

Note for Raspberry Pi 4 owners: the old versions of OS/toolchains produce broken binaries. Make sure to use latest OS! (see #226)

Optionally, you may install [`cfg_me`](https://github.com/Kixunil/cfg_me) tool for generating the manual page. The easiest way is to run `cargo install cfg_me`.

### Build

First build should take ~20 minutes:
```bash
$ git clone https://github.com/romanz/electrs
$ cd electrs
$ cargo build --release
```

If you installed `cfg_me` to generate man page, you can run `cfg_me man` to see it right away or `cfg_me -o electrs.1 man` to save it into a file (`electrs.1`).

## Docker-based installation from source

```bash
$ docker build -t electrs-app .
$ docker run --network host \
             --volume $HOME/.bitcoin:/home/user/.bitcoin:ro \
             --volume $PWD:/home/user \
             --rm -i -t electrs-app \
             electrs -vvvv --timestamp --db-dir /home/user/db
```

## Native OS packages

There are currently no official/stable binary pckages.

However, there's an [**experimental** repository for Debian 10](https://deb.ln-ask.me) (should work on recent Ubuntu, but not tested well-enough). The repository provides several significant advantages:

* Everything is completely automatic - after installing `electrs` via `apt`, it's running and will automatically run on reboot, restart after crash... It also connects to bitoind out-of-the-box, no messing with config files or anything else. It just works.
* Prebuilt binaries save you a lot of time. The binary installation of all the components is under 3 minutes on common hardware. Building from source is much longer.
* The repository contains some seurity hardening out-of-the-box - separate users for services, use of [btc-rpc-proxy](https://github.com/Kixunil/btc-rpc-proxy), etc.

And two significant disadvantages:

* It's currently impossible to independently verify the built packages, so you have to trust the author of the repository. This will hopefully change in the future.
* The repository is considered experimental and not well tested yet. The author of the repository is also a contributor to `electrs` and appreciates [bug reports](https://github.com/Kixunil/cryptoanarchy-deb-repo-builder/issues), [test reports](https://github.com/Kixunil/cryptoanarchy-deb-repo-builder/issues/61), and other contributions.

## Manual configuration

This applies only if you do **not** use some other automated systems such as Debian packages. If you use automated systems, refer to their documentation first!

### Bitcoind configuration

Pruning must be turned **off** for `electrs` to work. `txindex` is allowed but unnecessary for `electrs`. However, you might still need it if you run other services (e.g.`eclair`)

The highly recommended way of authenticating `electrs` is using cookie file. It's the most secure and robust method. Set `rpccookiefile` option of `bitcoind` to a file within an existing directory which it can access. You can skip it if you're running both daemons under the same user and with the default directories.

`electrs` will wait for `bitcoind` to sync, however, you will be unabe to use it until the syncing is done.

Example command for running `bitcoind` (assuming same user, default dirs):

```bash
$ bitcoind -server=1 -txindex=0 -prune=0
```
### Electrs configuration

Electrs can be configured using command line, environment variables and configuration files (or their combination). It is highly recommended to use configuration files for any non-trivial setups since it's easier to manage. If you're setting password manually instead of cookie files, configuration file is the only way to set it due to security reasons.

### Configuration files and priorities

The config files must be in the Toml format. These config files are (from lowest priority to highest): `/etc/electrs/config.toml`, `~/.electrs/config.toml`, `./electrs.toml`.

The options in highest-priority config files override options set in lowest-priority config files. Environment variables override options in config files and finally arguments override everythig else. There are two special arguments `--conf` which reads the specified file and `--conf-dir`, which read all the files in the specified directory. The options in those files override **everything that was set previously, including arguments that were passed before these arguments**. In general, later arguments override previous ones. It is a good practice to use these special arguments at the beginning of the command line in order to avoid confusion.

For each command line argument an environment variable of the same name with `ELECTRS_` prefix, upper case letters and underscores instead of hypens exists (e.g. you can use `ELECTRS_ELECTRUM_RPC_ADDR` instead of `--electrum-rpc-addr`). Similarly, for each such argument an option in config file exists with underscores instead of hypens (e.g. `electrum_rpc_addr`). In addition, config files support `cookie` option to specify cookie - this is not available using command line or environment variables for security reasons (other applications could read it otherwise). Note that this is different from using `cookie_path`, which points to a file containing the cookie instead of being the cookie itself.

Finally, you need to use a number in config file if you want to increase verbosity (e.g. `verbose = 3` is equivalent to `-vvv`) and `true` value in case of flags (e.g. `timestamp = true`)

If you are using `-rpcuser=USER` and `-rpcpassword=PASSWORD` of `bitcoind` for authentication, please use `cookie="USER:PASSWORD"` option in one of the [config files](https://github.com/romanz/electrs/blob/master/doc/usage.md#configuration-files-and-priorities).
Otherwise, [`~/.bitcoin/.cookie`](https://github.com/bitcoin/bitcoin/blob/0212187fc624ea4a02fc99bc57ebd413499a9ee1/contrib/debian/examples/bitcoin.conf#L70-L72) will be used as the default cookie file, allowing this server to use bitcoind JSONRPC interface.

### Electrs usage

First index sync should take ~1.5 hours (on a dual core Intel CPU @ 3.3 GHz, 8 GB RAM, 1TB WD Blue HDD):
```bash
$ cargo run --release -- -vvv --timestamp --db-dir ./db --electrum-rpc-addr="127.0.0.1:50001"
2018-08-17T18:27:42 - INFO - NetworkInfo { version: 179900, subversion: "/Satoshi:0.17.99/" }
2018-08-17T18:27:42 - INFO - BlockchainInfo { chain: "main", blocks: 537204, headers: 537204, bestblockhash: "0000000000000000002956768ca9421a8ddf4e53b1d81e429bd0125a383e3636", pruned: false, initialblockdownload: false }
2018-08-17T18:27:42 - DEBUG - opening DB at "./db/mainnet"
2018-08-17T18:27:42 - DEBUG - full compaction marker: None
2018-08-17T18:27:42 - INFO - listing block files at "/home/user/.bitcoin/blocks/blk*.dat"
2018-08-17T18:27:42 - INFO - indexing 1348 blk*.dat files
2018-08-17T18:27:42 - DEBUG - found 0 indexed blocks
2018-08-17T18:27:55 - DEBUG - applying 537205 new headers from height 0
2018-08-17T19:31:01 - DEBUG - no more blocks to index
2018-08-17T19:31:03 - DEBUG - no more blocks to index
2018-08-17T19:31:03 - DEBUG - last indexed block: best=0000000000000000002956768ca9421a8ddf4e53b1d81e429bd0125a383e3636 height=537204 @ 2018-08-17T15:24:02Z
2018-08-17T19:31:05 - DEBUG - opening DB at "./db/mainnet"
2018-08-17T19:31:06 - INFO - starting full compaction
2018-08-17T19:58:19 - INFO - finished full compaction
2018-08-17T19:58:19 - INFO - enabling auto-compactions
2018-08-17T19:58:19 - DEBUG - opening DB at "./db/mainnet"
2018-08-17T19:58:26 - DEBUG - applying 537205 new headers from height 0
2018-08-17T19:58:27 - DEBUG - downloading new block headers (537205 already indexed) from 000000000000000000150d26fcc38b8c3b71ae074028d1d50949ef5aa429da00
2018-08-17T19:58:27 - INFO - best=000000000000000000150d26fcc38b8c3b71ae074028d1d50949ef5aa429da00 height=537218 @ 2018-08-17T16:57:50Z (14 left to index)
2018-08-17T19:58:28 - DEBUG - applying 14 new headers from height 537205
2018-08-17T19:58:29 - INFO - RPC server running on 127.0.0.1:50001
```
You can specify options via command-line parameters, environment variables or using config files. See the documentation above.

Note that the final DB size should be ~20% of the `blk*.dat` files, but it may increase to ~35% at the end of the inital sync (just before the [full compaction is invoked](https://github.com/facebook/rocksdb/wiki/Manual-Compaction)).

If initial sync fails due to `memory allocation of xxxxxxxx bytes failedAborted` errors, as may happen on devices with limited RAM, try the following arguments when starting `electrs`. It should take roughly 18 hours to sync and compact the index on an ODROID-HC1 with 8 CPU cores @ 2GHz, 2GB RAM, and an SSD using the following command:

```bash
$ cargo run --release -- -vvvv --index-batch-size=10 --jsonrpc-import --db-dir ./db --electrum-rpc-addr="127.0.0.1:50001"
```

The index database is stored here:
```bash
$ du db/
38G db/mainnet/
```

See below for [extra configuration suggestions](https://github.com/romanz/electrs/blob/master/doc/usage.md#extra-configuration-suggestions) that you might want to consider.

## Electrum client

If you happen to use the Electrum client from [the **experimental** Debian repository](https://github.com/romanz/electrs/blob/master/doc/usage.md#cnative-os-packages), it's pre-configured out-of-the-box already. Read below otherwise.

There's a prepared script for launching `electrum` in such way to connect only to the local `electrs` instance to protect your privacy.

```bash
$ ./scripts/local-electrum.bash
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

## Extra configuration suggestions

### SSL connection

In order to use a secure connection, you can also use [NGINX as an SSL endpoint](https://docs.nginx.com/nginx/admin-guide/security-controls/terminating-ssl-tcp/#) by placing the following block in `nginx.conf`.

```nginx
stream {
        upstream electrs {
                server 127.0.0.1:50001;
        }

        server {
                listen 50002 ssl;
                proxy_pass electrs;

                ssl_certificate /path/to/example.crt;
                ssl_certificate_key /path/to/example.key;
                ssl_session_cache shared:SSL:1m;
                ssl_session_timeout 4h;
                ssl_protocols TLSv1 TLSv1.1 TLSv1.2 TLSv1.3;
                ssl_prefer_server_ciphers on;
        }
}
```

```bash
$ sudo systemctl restart nginx
$ electrum --oneserver --server=example:50002:s
```

Note: If you are connecting to electrs from Eclair Mobile or another similar client which does not allow self-signed SSL certificates, you can obtain a free SSL certificate as follows:

1. Follow the instructions at https://certbot.eff.org/ to install the certbot on your system.
2. When certbot obtains the SSL certificates for you, change the SSL paths in the nginx template above as follows:
```
ssl_certificate /etc/letsencrypt/live/<your-domain>/fullchain.pem;
ssl_certificate_key /etc/letsencrypt/live/<your-domain>/privkey.pem;
```

### Tor hidden service

Install Tor on your server and client machines (assuming Ubuntu/Debian):

```
$ sudo apt install tor
```

Add the following config to `/etc/tor/torrc`:
```
HiddenServiceDir /var/lib/tor/electrs_hidden_service/
HiddenServiceVersion 3
HiddenServicePort 50001 127.0.0.1:50001
```

If you use [the **experimental** Debian repository](https://github.com/romanz/electrs/blob/master/doc/usage.md#cnative-os-packages), it is cleaner to install `tor-hs-patch-config` using `apt` and then placing the configuration into a file inside `/etc/tor/hidden-services.d`.

Restart the service:
```
$ sudo systemctl restart tor
```

Note: your server's onion address is stored under:
```
$ sudo cat /var/lib/tor/electrs_hidden_service/hostname
<your-onion-address>.onion
```

On your client machine, run the following command (assuming Tor proxy service runs on port 9050):
```
$ electrum --oneserver --server <your-onion-address>.onion:50001:t --proxy socks5:127.0.0.1:9050
```

For more details, see http://docs.electrum.org/en/latest/tor.html.

### Sample Systemd Unit File

If you use [the **experimental** Debian repository](https://github.com/romanz/electrs/blob/master/doc/usage.md#cnative-os-packages), you should skip this section, as the appropriate systemd unit file is installed automatically.

You may wish to have systemd manage electrs so that it's "always on." Here is a sample unit file (which assumes that the bitcoind unit file is `bitcoind.service`):

```
[Unit]
Description=Electrs
After=bitcoind.service

[Service]
WorkingDirectory=/home/bitcoin/electrs
ExecStart=/home/bitcoin/electrs/target/release/electrs --db-dir ./db --electrum-rpc-addr="127.0.0.1:50001"
User=bitcoin
Group=bitcoin
Type=simple
KillMode=process
TimeoutSec=60
Restart=always
RestartSec=60

[Install]
WantedBy=multi-user.target


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
