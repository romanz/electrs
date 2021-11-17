#!/usr/bin/env python3
import argparse
import base58
import hashlib
import sys

from logbook import Logger, StreamHandler

import client

log = Logger("get_balance")


prefix_dict = {
    'mainnet': {
        'xpub': '0488b21e',  # P2PKH or P2SH  - m/44'/0'
        'ypub': '049d7cb2',  # P2WPKH in P2SH - m/49'/0'
        'zpub': '04b24746',  # P2WPKH         - m/84'/0'
        },
    'testnet': {
        'tpub': '043587cf',  # P2PKH or P2SH  - m/44'/1'
        'upub': '044a5262',  # P2WPKH in P2SH - m/49'/1'
        'vpub': '045f1cf6',  # P2WPKH         - m/84'/1'
    },
    'regtest': {
    },
}


def convert_key(key, target_prefix, network_name):
    decoded_key_bytes = base58.b58decode_check(key)
    target_key_bytes = (
        bytes.fromhex(prefix_dict[network_name][target_prefix]) +
        decoded_key_bytes[4:])
    return base58.b58encode_check(target_key_bytes).decode('ascii')


def compute_xpub_balance(xpub, conn, network, details):
    total = 0
    for change in (0, 1):
        empty = 0
        for n in range(1000):
            address = xpub.subkey(change).subkey(n).address()
            script = network.parse.address(address).script()
            script_hash = hashlib.sha256(script).digest()[::-1].hex()
            # conn.call([client.request('blockchain.scripthash.subscribe',
            #                           script_hash)])
            result, = conn.call(
                [client.request('blockchain.scripthash.get_history',
                                script_hash)])
            ntx = len(result)
            if len(result):
                log.debug(result)
            result, = conn.call(
                [client.request('blockchain.scripthash.get_balance',
                                script_hash)])
            confirmed = result['confirmed'] / 1e8
            total += confirmed

            log.debug(
                '{}/{}: {} -> {} BTC confirmed, {} BTC unconfirmed, '
                '{} txs balance = {} BTC', change, n, address,
                result["confirmed"] / 1e8, result["unconfirmed"] / 1e8, ntx,
                total)

            if confirmed or ntx:
                empty = 0
                if confirmed > 0:
                    details[address] = confirmed
            else:
                empty += 1
                if empty >= 10:
                    break
    return total


def compute_address_balance(address, conn, network):
    script = network.parse.address(address).script()
    script_hash = hashlib.sha256(script).digest()[::-1].hex()
    result, = conn.call(
        [client.request('blockchain.scripthash.get_balance',
                        script_hash)])
    return result['confirmed'] / 1e8


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--host', default='localhost')
    parser.add_argument('--network', default='mainnet',
                        choices=['mainnet', 'testnet', 'regtest'])
    parser.add_argument('address')
    args = parser.parse_args()

    if args.network == 'regtest':
        port = 60401
        from pycoin.symbols.xrt import network
    elif args.network == 'testnet':
        port = 60001
        from pycoin.symbols.xtn import network
    elif args.network == 'mainnet':
        port = 50001
        from pycoin.symbols.btc import network
    else:
        raise ValueError(f"unknown network: {args.network}")

    conn = client.Client((args.host, port))
    total = 0
    xpub = (network.parse.bip32(args.address) or
            network.parse.bip49(args.address) or
            network.parse.bip84(args.address))

    if xpub is None:
        total = compute_address_balance(args.address, conn, network)
    else:
        details = {}
        total = compute_xpub_balance(xpub, conn, network, details)

        for prefix in prefix_dict[args.network]:
            if args.address[:4] != prefix:
                key = convert_key(args.address, prefix, args.network)
                log.debug('Trying with {}', key)
                xpub = (network.parse.bip32(key) or network.parse.bip49(key)
                        or network.parse.bip84(key))
                total += compute_xpub_balance(xpub, conn, network, details)

        for addr in details:
            log.info('{} balance: {} BTC', addr, details[addr])

    log.info('total balance: {} BTC', total)


if __name__ == '__main__':
    with StreamHandler(sys.stderr, level='DEBUG').applicationbound():
        main()
