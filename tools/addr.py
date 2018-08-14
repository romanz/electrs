#!/usr/bin/env python3
import hashlib
import sys

from pycoin.coins.bitcoin.networks import BitcoinMainnet

import client

def main():
    conn = client.Connection(('localhost', 50001))
    addr, = sys.argv[1:]
    script = BitcoinMainnet.ui.script_for_address(addr)
    script_hash = hashlib.sha256(script).digest()[::-1].hex()
    res = conn.call('blockchain.scripthash.get_balance', script_hash)['result']
    print('{} has {} satoshis'.format(addr, res))


if __name__ == '__main__':
    main()
