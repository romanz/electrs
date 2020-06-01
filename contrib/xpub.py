#!/usr/bin/env python3
import hashlib
import sys

from logbook import Logger, StreamHandler

from pycoin.symbols.btc import network

import client

log = Logger("xpub")

def main():
    conn = client.Client(('localhost', 50001))
    xpub, = sys.argv[1:]
    total = 0
    xpub = network.parse.bip32_pub(xpub)

    for change in (0, 1):
        empty = 0
        for n in range(100):
            address = xpub.subkey(change).subkey(n).address()
            script = network.parse.address(address).script()
            script_hash = hashlib.sha256(script).digest()[::-1].hex()
            log.debug('{}', conn.call('blockchain.scripthash.get_history',
                                      script_hash))
            reply = conn.call('blockchain.scripthash.get_balance', script_hash)
            result = reply['result']
            confirmed = result['confirmed'] / 1e8
            total += confirmed
            if confirmed:
                log.info('{}/{} => {} has {:11.8f} BTC',
                         change, n, address, confirmed)
                empty = 0
            else:
                empty += 1
                if empty >= 10:
                    break
    log.info('total balance: {} BTC', total)


if __name__ == '__main__':
    with StreamHandler(sys.stderr, level='INFO').applicationbound():
        main()
