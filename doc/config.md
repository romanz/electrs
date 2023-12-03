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

`electrs` will wait for `bitcoind` to sync, however, you will be unable to use it until the syncing is done.

Example command for running `bitcoind` (assuming same user, default dirs):

```bash
$ bitcoind -server=1 -txindex=0 -prune=0
```
### Electrs configuration

**Note:** this documentation may occasionally become stale. We recommend running `electrs --help` to get an up-to-date list of options.

Electrs can be configured using command line, environment variables and configuration files (or their combination).
It is highly recommended to use configuration files for any non-trivial setups since it's easier to manage.
If you're setting password manually instead of cookie files, configuration file is the only way to set it due to security reasons.

**Important:** you must configure `db_dir` to be either an empty directory or previously used by `electrs`!
The contents of this directory is considered **internal to `electrs`** and any tampering that is **not** explicitly allowed by documentation
can lead to serious problems! Currently the *only* permitted operation is *deleting whole `mainnet` subdirectory when upgrading to version 0.9.0* - see the upgrading section.

#### Configuration files and priorities

The Toml-formatted config files ([an example here](config_example.toml)) are (from lowest priority to highest): `/etc/electrs/config.toml`, `~/.electrs/config.toml`, `./electrs.toml`.
They are loaded if they *exist* and ignored if not however, to aid debugging, any other error when opening them such as permission error will make electrs exit with error.

The options in highest-priority config files override options set in lowest-priority config files.
If loading these files is undesirable (common in case of protected systemd services), use the `--skip-default-conf-files` argument to prevent it.

**Environment variables** override options in config files and finally **arguments** override everythig else.

There are two special arguments `--conf` which reads the specified file and `--conf-dir`, which read all the files in the specified directory.

The options in those files override **everything** that was set previously, **including arguments** that were passed before these two special arguments.

In general, later arguments override previous ones.
It is a good practice to use these special arguments at the beginning of the command line in order to avoid confusion.

**Naming convention**

For each command line argument an **environment variable** of the same name with `ELECTRS_` prefix, upper case letters and underscores instead of hyphens exists
(e.g. you can use `ELECTRS_ELECTRUM_RPC_ADDR` instead of `--electrum-rpc-addr`).

Similarly, for each such argument an option in config file exists with underscores instead of hyphens (e.g. `electrum_rpc_addr`).

You need to use `true` value in case of flags (e.g. `timestamp = true`).

**Authentication**

In addition, config files support `auth` option to specify username and password.
This is not available using command line or environment variables for security reasons (other applications could read it otherwise).
**Important note**: `auth` is different from `cookie_file`, which points to a file containing the cookie instead of being the cookie itself!

If you are using `-rpcuser=USER` and `-rpcpassword=PASSWORD` of `bitcoind` for authentication, please use `auth="USER:PASSWORD"` option in one of the [config files](config.md#configuration-files-and-priorities).
Otherwise, [`~/.bitcoin/.cookie`](https://github.com/bitcoin/bitcoin/blob/0212187fc624ea4a02fc99bc57ebd413499a9ee1/contrib/debian/examples/bitcoin.conf#L70-L72) will be used as the default cookie file,
allowing this server to use bitcoind JSONRPC interface.

Note: there was a `cookie` option in the version 0.8.7 and below, it's now deprecated - do **not** use, it will be removed.
Please read upgrade notes if you're upgrading to a newer version.

## Connecting an Electrum client ##

To connect to your Electrs server, you will need to point Electrum to your server using the `ip_address:port` syntax. You will notice that most default servers in Electrum use the `50002` port (which is for SSL connections), while Electrs serves port `50001` and does not provide SSL out of the box.

You would need to either use a webserver to provide SSL (see _SSL connection_ below), or connect without SSL. To tell Electrum to connect to your server without SSL, you need to add `:t` after the port (ie: `localhost:50001:t`). Please note that this is not secure and therefore recommended only for local connections.

Electrs will listen by default on `127.0.0.1:50001`, which means it will only serve clients in the local machine. This is configured via the `electrum_rpc_addr` setting and if you wish to connect from another machine, you need to change it to `0.0.0.0:50001`. This is less secure though, and the recommended way to access Electrs remotely is to keep listening on `127.0.0.1` and tunnel to your server.

## Extra configuration suggestions

### SSL connection

In order to use a secure connection, you can also use [NGINX as an SSL endpoint](https://docs.nginx.com/nginx/admin-guide/security-controls/terminating-ssl-tcp/#)
by placing the following block in `nginx.conf`.
Notice that while electrs doesn't use HTTP the configuration below uses raw TCP stream which works.

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

If you use [the *beta* Debian repository](binaries.md#cnative-os-packages),
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

If you use [the *beta* Debian repository](binaries.md#cnative-os-packages), you should skip this section,
as the appropriate systemd unit file is installed automatically.

You may wish to have systemd manage electrs so that it's "always on".
Here is a sample unit file (which assumes that the bitcoind unit file is `bitcoind.service`):

```
[Unit]
Description=Electrs
After=bitcoind.service

[Service]
WorkingDirectory=/home/bitcoin/electrs
ExecStart=/home/bitcoin/electrs/target/release/electrs --log-filters INFO --db-dir ./db --electrum-rpc-addr="127.0.0.1:50001"
User=bitcoin
Group=bitcoin
Type=simple
KillMode=process
TimeoutSec=60
Restart=always
RestartSec=60

Environment="RUST_BACKTRACE=1"

# Hardening measures
PrivateTmp=true
ProtectSystem=full
NoNewPrivileges=true
MemoryDenyWriteExecute=true

[Install]
WantedBy=multi-user.target
```
