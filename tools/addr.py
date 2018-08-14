#!/usr/bin/env python3
import hashlib
import sys
import argparse

from pycoin.coins.bitcoin.networks import BitcoinTestnet, BitcoinMainnet

import client

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--testnet', action='store_true')
    parser.add_argument('address', nargs='+')
    args = parser.parse_args()

    if args.testnet:
        Network = BitcoinTestnet
        port = 60001
    else:
        Network = BitcoinMainnet
        port = 50001

    conn = client.Connection(('localhost', port))
    for addr in args.address:
        script = Network.ui.script_for_address(addr)
        script_hash = hashlib.sha256(script).digest()[::-1].hex()
        reply = conn.call('blockchain.scripthash.get_balance', script_hash)
        result = reply['result']
        print('{} has {} satoshis'.format(addr, result))


if __name__ == '__main__':
    main()
