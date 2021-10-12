#!/usr/bin/env python3
import argparse
import client
import json

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--host', default='localhost')
    parser.add_argument("txid")
    args = parser.parse_args()

    conn = client.Client((args.host, 50001))
    tx, = conn.call([client.request("blockchain.transaction.get", args.txid, True)])
    print(json.dumps(tx))

if __name__ == "__main__":
    main()
