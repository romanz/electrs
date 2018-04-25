import hashlib
import sys

from logbook import Logger, StreamHandler

from pycoin.coins.bitcoin.networks import BitcoinMainnet
import pycoin.ui.key_from_text
import pycoin.key

import zmq

script_for_address = BitcoinMainnet.ui.script_for_address

log = Logger(__name__)


def main():
    c = zmq.Context()
    s = c.socket(zmq.REQ)
    s.connect('ipc:///tmp/indexrs.rpc')

    xpub, = sys.argv[1:]
    total = 0
    k = pycoin.ui.key_from_text.key_from_text(xpub)
    for change in (0, 1):
        empty = 0
        for n in range(100):
            address = k.subkey(change).subkey(n).address()
            script = script_for_address(address)
            script_hash = hashlib.sha256(script).digest()
            s.send(script_hash)
            res = s.recv()
            b = float(res)
            total += b
            if b:
                log.info('{}/{} => {} has {:11.8f} BTC',
                         change, n, address, b)
                empty = 0
            else:
                empty += 1
                if empty >= 10:
                    break
    log.info('total balance: {} BTC', total)

if __name__ == '__main__':
    with StreamHandler(sys.stderr, level='INFO').applicationbound():
        main()
