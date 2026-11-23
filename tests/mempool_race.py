#!/usr/bin/env python3
"""A block found during the mempool sync must not hide its transactions.

electrs syncs its index, then the mempool. A block mined in between drops its
transactions from bitcoind's mempool before the index has it, so electrs would
report them neither unconfirmed nor confirmed until its next sync.

A proxy between electrs and bitcoind makes it deterministic: it mines a block
with the mempool transactions right before forwarding electrs' `getrawmempool`.

Usage: tests/mempool_race.py --electrs <path> --bitcoind <path> [--data-dir <dir>]
"""

import argparse
import base64
import hashlib
import http.client
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_until(predicate, timeout, what):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if predicate():
                return
        except (OSError, ValueError, KeyError):
            pass
        time.sleep(0.2)
    raise TimeoutError(f"timed out waiting for {what}")


class Bitcoind:
    def __init__(self, path, datadir):
        self.datadir = datadir
        self.rpc_port = free_port()
        self.proc = subprocess.Popen(
            [
                path,
                "-regtest",
                "-rest",
                f"-datadir={datadir}",
                f"-rpcport={self.rpc_port}",
                f"-port={free_port()}",
                "-listen=0",
                "-fallbackfee=0.0001",
                "-printtoconsole=0",
            ]
        )
        wait_until(lambda: self.call("getblockcount") is not None, 60, "bitcoind")

    def call(self, method, *params, wallet=None):
        with open(os.path.join(self.datadir, "regtest", ".cookie")) as f:
            auth = base64.b64encode(f.read().strip().encode()).decode()
        conn = http.client.HTTPConnection("127.0.0.1", self.rpc_port, timeout=60)
        path = f"/wallet/{wallet}" if wallet else "/"
        body = json.dumps({"jsonrpc": "2.0", "id": 0, "method": method, "params": params})
        conn.request("POST", path, body, {"Authorization": f"Basic {auth}"})
        resp = json.loads(conn.getresponse().read())
        if resp.get("error"):
            raise ValueError(f"{method}: {resp['error']}")
        return resp["result"]

    def stop(self):
        try:
            self.call("stop")
        except OSError:
            pass
        self.proc.wait(timeout=60)


class Proxy(ThreadingHTTPServer):
    """Forward electrs' requests to bitcoind, mining a block before the armed `getrawmempool`."""

    daemon_threads = True

    def __init__(self, bitcoind, mine):
        super().__init__(("127.0.0.1", 0), ProxyHandler)
        self.bitcoind = bitcoind
        self.mine = mine
        self.armed = threading.Event()
        self.mined = threading.Event()
        self.lock = threading.Lock()
        threading.Thread(target=self.serve_forever, daemon=True).start()

    def on_request(self, body):
        if not self.armed.is_set():
            return
        reqs = json.loads(body)
        methods = {r["method"] for r in (reqs if isinstance(reqs, list) else [reqs])}
        with self.lock:
            if "getrawmempool" in methods and self.armed.is_set():
                self.armed.clear()
                self.mine()
                self.mined.set()


class ProxyHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        self.forward(None)

    def do_POST(self):
        body = self.rfile.read(int(self.headers["Content-Length"]))
        self.server.on_request(body)
        self.forward(body)

    def forward(self, body):
        conn = http.client.HTTPConnection("127.0.0.1", self.server.bitcoind.rpc_port, timeout=60)
        headers = {
            k: v for k, v in self.headers.items() if k.lower() in ("authorization", "content-type")
        }
        conn.request(self.command, self.path, body, headers)
        resp = conn.getresponse()
        data = resp.read()
        self.send_response(resp.status)
        if resp.getheader("Content-Type"):
            self.send_header("Content-Type", resp.getheader("Content-Type"))
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


class Electrum:
    def __init__(self, port):
        self.sock = socket.create_connection(("127.0.0.1", port), timeout=120)
        self.file = self.sock.makefile("rw")
        self.next_id = 0

    def call(self, method, *params):
        self.next_id += 1
        self.file.write(json.dumps({"id": self.next_id, "method": method, "params": params}) + "\n")
        self.file.flush()
        resp = json.loads(self.file.readline())
        while resp.get("id") != self.next_id:  # skip the subscription notifications
            resp = json.loads(self.file.readline())
        if resp.get("error"):
            raise ValueError(f"{method}: {resp['error']}")
        return resp["result"]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--electrs", required=True)
    parser.add_argument("--bitcoind", required=True)
    parser.add_argument("--data-dir")
    args = parser.parse_args()

    datadir = tempfile.mkdtemp(prefix="electrs-mempool-race-", dir=args.data_dir)
    os.makedirs(os.path.join(datadir, "bitcoin"))
    bitcoind = Bitcoind(args.bitcoind, os.path.join(datadir, "bitcoin"))
    electrs = None
    passed = False
    try:
        bitcoind.call("createwallet", "w")
        mining_addr = bitcoind.call("getnewaddress", wallet="w")
        bitcoind.call("generatetoaddress", 101, mining_addr)

        def mine():
            txids = bitcoind.call("getrawmempool")
            bitcoind.call("generateblock", mining_addr, txids)

        proxy = Proxy(bitcoind, mine)
        electrum_port = free_port()
        log = open(os.path.join(datadir, "electrs.log"), "w")
        electrs = subprocess.Popen(
            [
                args.electrs,
                "--network=regtest",
                f"--db-dir={os.path.join(datadir, 'electrs')}",
                f"--daemon-dir={bitcoind.datadir}",
                f"--daemon-rpc-addr=127.0.0.1:{proxy.server_address[1]}",
                f"--electrum-rpc-addr=127.0.0.1:{electrum_port}",
                f"--monitoring-addr=127.0.0.1:{free_port()}",
                "--log-filters=info",
            ],
            stderr=log,
        )
        wait_until(lambda: socket.create_connection(("127.0.0.1", electrum_port)).close() is None,
                   60, "electrs Electrum RPC")
        client = Electrum(electrum_port)
        wait_until(lambda: client.call("blockchain.headers.subscribe")["height"] == 101,
                   300, "electrs initial sync")

        addr = bitcoind.call("getnewaddress", wallet="w")
        spk = bitcoind.call("getaddressinfo", addr, wallet="w")["scriptPubKey"]
        scripthash = hashlib.sha256(bytes.fromhex(spk)).digest()[::-1].hex()
        txid = bitcoind.call("sendtoaddress", addr, 1, wallet="w")

        def history():
            entries = client.call("blockchain.scripthash.get_history", scripthash)
            return [(e["tx_hash"], e["height"]) for e in entries]

        wait_until(lambda: history() == [(txid, 0)], 60, "the payment in electrs' mempool")
        print(f"payment {txid} seen in the mempool")

        proxy.armed.set()
        if not proxy.mined.wait(timeout=60):
            raise TimeoutError("electrs did not sync its mempool")
        # Served once the sync during which the block was mined is over.
        after = history()
        print(f"block 102 mined during the mempool sync, history: {after}")
        if after != [(txid, 102)]:
            print(f"FAIL: the payment should be confirmed at height 102, logs in {datadir}")
            return 1
        print("PASS")
        passed = True
        return 0
    finally:
        if electrs:
            electrs.terminate()
            electrs.wait(timeout=60)
        bitcoind.stop()
        if passed:
            shutil.rmtree(datadir)


if __name__ == "__main__":
    sys.exit(main())
