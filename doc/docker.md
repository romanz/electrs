# Docker installation

## Docker-based installation from source

**Important**: The `Dockerfile` is provided for demonstration purposes and may
NOT be suitable for production use. The maintainers of electrs are not deeply
familiar with Docker, so you should DYOR. If you are not familiar with Docker
either it's probably be safer to NOT use it.

Note: currently Docker installation links statically

Note: health check only works if Prometheus is running on port 4224 inside
container

```bash
$ docker build -t electrs-app .
$ mkdir db
$ docker run --network host \
             --volume $HOME/.bitcoin:/home/user/.bitcoin:ro \
             --volume $PWD/db:/home/user/db \
             --env ELECTRS_DB_DIR=/home/user/db \
             --rm -i -t electrs-app
```

If not using the host-network, you probably want to expose the ports for electrs
and Prometheus like so:

```bash
$ docker run --volume $HOME/.bitcoin:/home/user/.bitcoin:ro \
             --volume $PWD/db:/home/user/db \
             --env ELECTRS_DB_DIR=/home/user/db \
             --env ELECTRS_ELECTRUM_RPC_ADDR=0.0.0.0:50001 \
             --env ELECTRS_MONITORING_ADDR=0.0.0.0:4224 \
             --rm -i -t electrs-app
```

To access the server from outside Docker, add `-p 50001:50001 -p 4224:4224` but
be aware of the security risks. Good practice is to group containers that needs
access to the server inside the same Docker network and not expose the ports to
the outside world.

## Docker compose, one step for installation

**NOTE**: This is intend for one step setup with testnet/signet/regnet, inspired
by [Polar](https://github.com/jamaljsr/polar). **NOT RECOMMAND WITH MAINNET**

To setup testnet, just switch to the [docker](../docker/testnet), and then run:

```bash
docker compose up -d
```

If you are not using Docker Desktop, run:

```bash
docker-compose up -d
```

The default rpc auth info for bitcoin core is: `alice:alice`, if you want to
change it, just create a new one with [rpcauth.py](https://github.com/bitcoin/bitcoin/tree/master/share/rpcauth)
and replace both line 19 in `docker-compose.yml` and line 16 in `electrs.toml`
with new generated auth info:

```
-rpcauth=alice:128f22494cc2208ea8376a3d0b45a131$9cc0187c0e49f35454a3ed1250e40ecaef420cfcd294d3ac22496adbe64f04b9
```

Using it with bitcoin mainnet highly not recommand, use it with you own risk.
It can not start properly with mainnet by default, you need to generated new rpc
auth info and modify `docker-compose.yml` and `electrs.toml`.
