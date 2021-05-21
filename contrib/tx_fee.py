#!/usr/bin/env python3
import argparse
import client

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("txid")
    args = parser.parse_args()

    conn = client.Client(("localhost", 50001))
    tx = conn.call(conn.request("blockchain.transaction.get", args.txid, True))[0]["result"]
    requests = []
    for vin in tx["vin"]:
        prev_txid = vin["txid"]
        requests.append(conn.request("blockchain.transaction.get", prev_txid, True))

    fee = 0
    for vin, response in zip(tx["vin"], conn.call(*requests)):
        prev_tx = response["result"]
        txo = prev_tx["vout"][vin["vout"]]
        fee += txo["value"]

    fee -= sum(vout["value"] for vout in tx["vout"])

    print(f'vSize = {tx["vsize"]}, Fee = {1e3 * fee:.2f} mBTC = {1e8 * fee / tx["vsize"]:.2f} sat/vB')

if __name__ == "__main__":
    main()
