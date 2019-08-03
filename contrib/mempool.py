#!/usr/bin/env python3

import argparse
from daemon import Daemon

import numpy as np
import matplotlib.pyplot as plt


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
    entries = [{'fee': e['fee']*1e8, 'vsize': e['vsize']} for e in entries]
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
