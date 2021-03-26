#!/usr/bin/env python3
import hashlib
import sys
import argparse

import client

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--testnet', action='store_true')
    parser.add_argument('address', nargs='+')
    args = parser.parse_args()

    if args.testnet:
        port = 60001
        from pycoin.symbols.xtn import network
    else:
        port = 50001
        from pycoin.symbols.btc import network

    conn = client.Client(('localhost', port))
    for addr in args.address:
        script = network.parse.address(addr).script()
        script_hash = hashlib.sha256(script).digest()[::-1].hex()
        reply = conn.call('blockchain.scripthash.subscribe', script_hash)
        print(f'{reply}')
        reply = conn.call('blockchain.scripthash.get_history', script_hash)
        result = reply['result']
        print(f'{addr} has {len(result)} transactions:')
        for tx in result:
            print(f'* {tx["tx_hash"]}')


if __name__ == '__main__':
    main()
