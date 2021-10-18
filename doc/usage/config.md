## Manual configuration

This applies only if you do **not** use some other automated systems such as Debian packages.
If you use automated systems, refer to their documentation first!

### Bitcoind configuration

Pruning must be turned **off** for `electrs` to work.
`txindex` is allowed but unnecessary for `electrs`.
However, you might still need it if you run other services (e.g.`eclair`).
The option `maxconnections` (if used) should be set to 12 or more for bitcoind to accept inbound p2p connections.
Note that setting `maxuploadtarget` may cause p2p-based sync to fail - so consider using `-whitelist=download@127.0.0.1` to disable the limit for local p2p connections.

The highly recommended way of authenticating `electrs` is using cookie file.
It's the most [secure](https://github.com/Kixunil/security_writings/blob/master/cookie_files.md) and robust method.
Set `rpccookiefile` option of `bitcoind` to a file within an existing directory which it can access.
You can skip it if you're running both daemons under the same user and with the default directories.

`electrs` will wait for `bitcoind` to sync, however, you will be unabe to use it until the syncing is done.

Example command for running `bitcoind` (assuming same user, default dirs):

```bash
$ bitcoind -server=1 -txindex=0 -prune=0
```
### Electrs configuration

Electrs can be configured using command line, environment variables and configuration files (or their combination).
It is highly recommended to use configuration files for any non-trivial setups since it's easier to manage.
If you're setting password manually instead of cookie files, configuration file is the only way to set it due to security reasons.

**Important:** you must configure `db_dir` to be either an empty directory or previously used by `electrs`!
The contents of this directory is considered **internal to `electrs`** and any tampering that is **not** explicitly allowed by documentation
can lead to serious problems! Currently the *only* permitted operation is *deleting whole `mainnet` subdirectory when upgrading to version 0.9.0* - see the upgrading section.

#### Configuration files and priorities

The Toml-formatted config files ([an example here](config_example.toml)) are (from lowest priority to highest): `/etc/electrs/config.toml`, `~/.electrs/config.toml`, `./electrs.toml`.

The options in highest-priority config files override options set in lowest-priority config files.

**Environment variables** override options in config files and finally **arguments** override everythig else.

There are two special arguments `--conf` which reads the specified file and `--conf-dir`, which read all the files in the specified directory.

The options in those files override **everything** that was set previously, **including arguments** that were passed before these two special arguments.

In general, later arguments override previous ones.
It is a good practice to use these special arguments at the beginning of the command line in order to avoid confusion.

**Naming convention**

For each command line argument an **environment variable** of the same name with `ELECTRS_` prefix, upper case letters and underscores instead of hypens exists
(e.g. you can use `ELECTRS_ELECTRUM_RPC_ADDR` instead of `--electrum-rpc-addr`).

Similarly, for each such argument an option in config file exists with underscores instead of hypens (e.g. `electrum_rpc_addr`).

You need to use a number in config file if you want to increase verbosity (e.g. `verbose = 3` is equivalent to `-vvv`) and `true` value in case of flags (e.g. `timestamp = true`)

**Authentication**

In addition, config files support `auth` option to specify username and password.
This is not available using command line or environment variables for security reasons (other applications could read it otherwise).
**Important note**: `auth` is different from `cookie_file`, which points to a file containing the cookie instead of being the cookie itself!

If you are using `-rpcuser=USER` and `-rpcpassword=PASSWORD` of `bitcoind` for authentication, please use `auth="USER:PASSWORD"` option in one of the [config files](https://github.com/romanz/electrs/blob/master/doc/usage.md#configuration-files-and-priorities).
Otherwise, [`~/.bitcoin/.cookie`](https://github.com/bitcoin/bitcoin/blob/0212187fc624ea4a02fc99bc57ebd413499a9ee1/contrib/debian/examples/bitcoin.conf#L70-L72) will be used as the default cookie file,
allowing this server to use bitcoind JSONRPC interface.

Note: there was a `cookie` option in the version 0.8.7 and below, it's now deprecated - do **not** use, it will be removed.
Please read upgrade notes if you're upgrading to a newer version.

### Electrs usage

First index sync should take ~4 hours for ~336GB @ August 2021 (on a dual core Intel CPU @ 3.3 GHz, 8 GB RAM, 1TB WD Blue HDD):
```bash
$ du -ch ~/.bitcoin/blocks/blk*.dat | tail -n1
336G  total

$ ./target/release/electrs -vv --timestamp --db-dir ./db --electrum-rpc-addr="127.0.0.1:50001"
Config { network: Bitcoin, db_path: "./db/bitcoin", daemon_dir: "/home/user/.bitcoin", daemon_auth: CookieFile("/home/user/.bitcoin/.cookie"), daemon_rpc_addr: V4(127.0.0.1:8332), daemon_p2p_addr: V4(127.0.0.1:8333), electrum_rpc_addr: V4(127.0.0.1:50001), monitoring_addr: V4(127.0.0.1:4224), wait_duration: 10s, index_batch_size: 10, index_lookup_limit: 100, ignore_mempool: false, server_banner: "Welcome to electrs 0.9.0 (Electrum Rust Server)!", args: [] }
[2021-08-17T18:48:40.054Z INFO  electrs::metrics::metrics_impl] serving Prometheus metrics on 127.0.0.1:4224
[2021-08-17T18:48:40.944Z INFO  electrs::db] "./db/bitcoin": 0 SST files, 0 GB, 0 Grows
[2021-08-17T18:48:41.075Z INFO  electrs::index] indexing 2000 blocks: [1..2000]
[2021-08-17T18:48:41.610Z INFO  electrs::chain] chain updated: tip=00000000dfd5d65c9d8561b4b8f60a63018fe3933ecb131fb37f905f87da951a, height=2000
[2021-08-17T18:48:41.623Z INFO  electrs::index] indexing 2000 blocks: [2001..4000]
[2021-08-17T18:48:42.178Z INFO  electrs::chain] chain updated: tip=00000000922e2aa9e84a474350a3555f49f06061fd49df50a9352f156692a842, height=4000
[2021-08-17T18:48:42.188Z INFO  electrs::index] indexing 2000 blocks: [4001..6000]
[2021-08-17T18:48:42.714Z INFO  electrs::chain] chain updated: tip=00000000dbbb79792303bdd1c6c4d7ab9c21bba0667213c2eca955e11230c5a5, height=6000
[2021-08-17T18:48:42.723Z INFO  electrs::index] indexing 2000 blocks: [6001..8000]
[2021-08-17T18:48:43.235Z INFO  electrs::chain] chain updated: tip=0000000094fbacdffec05aea9847000522a258c269ae37a74a818afb96fc27d9, height=8000
[2021-08-17T18:48:43.246Z INFO  electrs::index] indexing 2000 blocks: [8001..10000]
[2021-08-17T18:48:43.768Z INFO  electrs::chain] chain updated: tip=0000000099c744455f58e6c6e98b671e1bf7f37346bfd4cf5d0274ad8ee660cb, height=10000
<...>
[2021-08-17T22:11:20.139Z INFO  electrs::chain] chain updated: tip=00000000000000000002a23d6df20eecec15b21d32c75833cce28f113de888b7, height=690000
[2021-08-17T22:11:20.157Z INFO  electrs::index] indexing 2000 blocks: [690001..692000]
[2021-08-17T22:12:16.944Z INFO  electrs::chain] chain updated: tip=000000000000000000054dab4b85860fcee5808ab7357eb2bb45114a25b77380, height=692000
[2021-08-17T22:12:16.957Z INFO  electrs::index] indexing 2000 blocks: [692001..694000]
[2021-08-17T22:13:11.764Z INFO  electrs::chain] chain updated: tip=00000000000000000003f5acb5ec81df7c98c16bc8d89bdaadd4e8965729c018, height=694000
[2021-08-17T22:13:11.777Z INFO  electrs::index] indexing 2000 blocks: [694001..696000]
[2021-08-17T22:14:05.852Z INFO  electrs::chain] chain updated: tip=0000000000000000000dfc81671ac5a22d8751f9c1506689d3eaceaef26470b9, height=696000
[2021-08-17T22:14:05.855Z INFO  electrs::index] indexing 295 blocks: [696001..696295]
[2021-08-17T22:14:15.557Z INFO  electrs::chain] chain updated: tip=0000000000000000000eceb67a01c81c65b538a7b3729f879c6c1e248bb6577a, height=696295
[2021-08-17T22:14:21.578Z INFO  electrs::db] starting config compaction
[2021-08-17T22:14:21.623Z INFO  electrs::db] starting headers compaction
[2021-08-17T22:14:21.667Z INFO  electrs::db] starting txid compaction
[2021-08-17T22:22:27.009Z INFO  electrs::db] starting funding compaction
[2021-08-17T22:38:17.104Z INFO  electrs::db] starting spending compaction
[2021-08-17T22:55:11.785Z INFO  electrs::db] finished full compaction
[2021-08-17T22:55:15.835Z INFO  electrs::server] serving Electrum RPC on 127.0.0.1:50001
[2021-08-17T22:55:25.837Z INFO  electrs::index] indexing 7 blocks: [696296..696302]
[2021-08-17T22:55:26.120Z INFO  electrs::chain] chain updated: tip=0000000000000000000059e97dea0b0b9ebf4ac1fd66726b339fe1c9683de656, height=696302
[2021-08-17T23:02:03.453Z INFO  electrs::index] indexing 1 blocks: [696303..696303]
[2021-08-17T23:02:03.691Z INFO  electrs::chain] chain updated: tip=000000000000000000088107c337bf315e2db1e406c50566bd765f04a7e459b6, height=696303
```
You can specify options via command-line parameters, environment variables or using config files.
See the documentation above.

Note that the final DB size should be ~10% of the `blk*.dat` files, but it may increase to ~20% at the end of the inital sync (just before the [full compaction is invoked](https://github.com/facebook/rocksdb/wiki/Manual-Compaction)).

It should take roughly 18 hours to sync and compact the index on an ODROID-HC1 with 8 CPU cores @ 2GHz, 2GB RAM, and an SSD using the command above.

The index database is stored here:
```bash
$ du db/
30G db/mainnet/
```

See below for [extra configuration suggestions](https://github.com/romanz/electrs/blob/master/doc/usage.md#extra-configuration-suggestions) that you might want to consider.

## Electrum client

If you happen to use the Electrum client from [the *beta* Debian repository](https://github.com/romanz/electrs/blob/master/doc/usage.md#cnative-os-packages), it's pre-configured out-of-the-box already
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

## Extra configuration suggestions

### SSL connection

In order to use a secure connection, you can also use [NGINX as an SSL endpoint](https://docs.nginx.com/nginx/admin-guide/security-controls/terminating-ssl-tcp/#)
by placing the following block in `nginx.conf`.

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

If you use [the *beta* Debian repository](https://github.com/romanz/electrs/blob/master/doc/usage.md#cnative-os-packages),
it is cleaner to install `tor-hs-patch-config` using `apt` and then placing the configuration into a file inside `/etc/tor/hidden-services.d`.

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

If you use [the *beta* Debian repository](https://github.com/romanz/electrs/blob/master/doc/usage.md#cnative-os-packages), you should skip this section,
as the appropriate systemd unit file is installed automatically.

You may wish to have systemd manage electrs so that it's "always on".
Here is a sample unit file (which assumes that the bitcoind unit file is `bitcoind.service`):

```
[Unit]
Description=Electrs
After=bitcoind.service

[Service]
WorkingDirectory=/home/bitcoin/electrs
ExecStart=/home/bitcoin/electrs/target/release/electrs -vv --db-dir ./db --electrum-rpc-addr="127.0.0.1:50001"
User=bitcoin
Group=bitcoin
Type=simple
KillMode=process
TimeoutSec=60
Restart=always
RestartSec=60

[Install]
WantedBy=multi-user.target
```