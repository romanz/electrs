#!/usr/bin/env python3
import argparse
import client
import json

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("txid")
    args = parser.parse_args()

    conn = client.Client(("localhost", 50001))
    tx = conn.call("blockchain.transaction.get", args.txid, True)["result"]
    print(json.dumps(tx))

if __name__ == "__main__":
    main()
