#!/usr/bin/env python3
import argparse
import datetime
import hashlib
import io
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--network', default='mainnet')
    args = parser.parse_args()

    if args.network == 'regtest':
        from pycoin.symbols.xrt import network
    elif args.network == 'testnet':
        from pycoin.symbols.xtn import network
    elif args.network == 'mainnet':
        from pycoin.symbols.btc import network
    else:
        raise ValueError(f"unknown network: {args.network}")

    for line in sys.stdin:
        addr = line.strip()
        script = network.parse.address(addr).script()
        script_hash = hashlib.sha256(script).digest()
        print(script_hash[::-1].hex())


if __name__ == '__main__':
    main()