import hashlib
import json
import sys

from logbook import Logger, StreamHandler

from pycoin.coins.bitcoin.networks import BitcoinMainnet
import pycoin.ui.key_from_text
import pycoin.key

import socket

script_for_address = BitcoinMainnet.ui.script_for_address

log = Logger(__name__)

class Connection:
    def __init__(self, addr):
        self.s = socket.create_connection(addr)
        self.f = self.s.makefile('r')
        self.id = 0

    def call(self, method, *args):
        req = {
            'id': self.id,
            'method': method,
            'params': list(args),
        }
        msg = json.dumps(req) + '\n'
        self.s.sendall(msg.encode('ascii'))
        return json.loads(self.f.readline())


def main():
    conn = Connection(('localhost', 50001))
    xpub, = sys.argv[1:]
    total = 0
    k = pycoin.ui.key_from_text.key_from_text(xpub)
    for change in (0, 1):
        empty = 0
        for n in range(100):
            address = k.subkey(change).subkey(n).address()
            script = script_for_address(address)
            script_hash = hashlib.sha256(script).digest()
            log.debug('{}', conn.call('blockchain.scripthash.get_history',
                                      script_hash[::-1].hex()))
            reply = conn.call('blockchain.scripthash.get_balance',
                              script_hash[::-1].hex())
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
