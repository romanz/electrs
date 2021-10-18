## Docker-based installation from source

Note: currently Docker installation links statically

Note: health check only works if Prometheus is running on port 4224 inside container

```bash
$ docker build -t electrs-app .
$ mkdir db
$ docker run --network host \
             --volume $HOME/.bitcoin:/home/user/.bitcoin:ro \
             --volume $PWD/db:/home/user/db \
             --env ELECTRS_DB_DIR=/home/user/db \
             --rm -i -t electrs-app
```

If not using the host-network, you probably want to expose the ports for electrs and Prometheus like so:

```bash
$ docker run --volume $HOME/.bitcoin:/home/user/.bitcoin:ro \
             --volume $PWD/db:/home/user/db \
             --env ELECTRS_DB_DIR=/home/user/db \
             --env ELECTRS_ELECTRUM_RPC_ADDR=0.0.0.0:50001 \
             --env ELECTRS_MONITORING_ADDR=0.0.0.0:4224 \
             --rm -i -t electrs-app
```

To access the server from outside Docker, add `-p 50001:50001 -p 4224:4224` but be aware of the security risks. Good practice is to group containers that needs access to the server inside the same Docker network and not expose the ports to the outside world.