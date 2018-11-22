#!/usr/bin/env python3
import argparse
import binascii
import json
import os
import socket

import numpy as np
import matplotlib.pyplot as plt

class Daemon:
    def __init__(self, port, cookie_dir):
        self.sock = socket.create_connection(('localhost', port))
        self.fd = self.sock.makefile()
        path = os.path.join(os.path.expanduser(cookie_dir), '.cookie')
        cookie = binascii.b2a_base64(open(path, 'rb').read())
        self.cookie = cookie.decode('ascii').strip()
        self.index = 0

    def request(self, method, params_list):
        obj = [{"method": method, "params": params, "id": self.index}
               for params in params_list]
        request = json.dumps(obj)

        msg = ('POST / HTTP/1.1\n'
               'Authorization: Basic {}\n'
               'Content-Length: {}\n\n'
               '{}'.format(self.cookie, len(request), request))
        self.sock.sendall(msg.encode('ascii'))

        status = self.fd.readline().strip()
        while True:
            if self.fd.readline().strip():
                continue  # skip headers
            else:
                break  # next line will contain the response

        data = self.fd.readline().strip()
        replies = json.loads(data)
        for reply in replies:
            assert reply['error'] is None
            assert reply['id'] == self.index

        self.index += 1
        return [d['result'] for d in replies]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--testnet', action='store_true')
    args = parser.parse_args()

    if args.testnet:
        d = Daemon(port=18332, cookie_dir='~/.bitcoin/testnet3')
    else:
        d = Daemon(port=8332, cookie_dir='~/.bitcoin')

    txids, = d.request('getrawmempool', [[False]])
    txids = list(map(lambda a: [a], txids))

    entries = d.request('getmempoolentry', txids)
    entries = [{'fee': e['fee']*1e8, 'vsize': e['size']} for e in entries]
    for e in entries:
        e['rate'] = e['fee'] / e['vsize']  # sat/vbyte
    entries.sort(key=lambda e: e['rate'], reverse=True)

    vsize = np.array([e['vsize'] for e in entries]).cumsum()
    rate = np.array([e['rate'] for e in entries])

    plt.semilogy(vsize / 1e6, rate, '-')
    plt.xlabel('Mempool size (MB)')
    plt.ylabel('Fee rate (sat/vbyte)')
    plt.title('{} transactions'.format(len(entries)))
    plt.grid()
    plt.show()


if __name__ == '__main__':
    main()
