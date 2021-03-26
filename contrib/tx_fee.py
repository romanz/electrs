#!/usr/bin/env python3
import argparse
import client

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("txid")
    args = parser.parse_args()

    conn = client.Client(("localhost", 50001))
    tx = conn.call("blockchain.transaction.get", args.txid, True)["result"]
    fee = 0
    for vin in tx["vin"]:
        prev_txid = vin["txid"]
        prev_tx = conn.call("blockchain.transaction.get", prev_txid, True)["result"]
        txo = prev_tx["vout"][vin["vout"]]
        fee += txo["value"]
    fee -= sum(vout["value"] for vout in tx["vout"])

    print(f'vSize = {tx["vsize"]}, Fee = {1e3 * fee:.2f} mBTC = {1e8 * fee / tx["vsize"]:.2f} sat/vB')

if __name__ == "__main__":
    main()
