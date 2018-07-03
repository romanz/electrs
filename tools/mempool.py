import binascii
import json
import socket

import numpy as np
import matplotlib.pyplot as plt

s = socket.create_connection(('localhost', 8332))
r = s.makefile()
cookie = binascii.b2a_base64(open('/home/roman/.bitcoin/.cookie', 'rb').read())
cookie = cookie.decode('ascii').strip()

def request(method, params_list):
    obj = [{"method": method, "params": params} for params in params_list]
    request = json.dumps(obj)

    msg = ('POST / HTTP/1.1\nAuthorization: Basic {}\nContent-Length: {}\n\n'
           '{}'.format(cookie, len(request), request))
    s.sendall(msg.encode('ascii'))

    status = r.readline().strip()
    headers = []
    while True:
        line = r.readline().strip()
        if line:
            headers.append(line)
        else:
            break

    data = r.readline().strip()
    replies = json.loads(data)
    assert all(r['error'] is None for r in replies), replies
    return [d['result'] for d in replies]


def main():
    txids, = request('getrawmempool', [[False]])
    txids = list(map(lambda a: [a], txids))

    entries = request('getmempoolentry', txids)
    entries = [{'fee': e['fee']*1e8, 'vsize': e['size']} for e in entries]
    for e in entries:
        e['rate'] = e['fee'] / e['vsize']  # sat/vbyte
    entries.sort(key=lambda e: e['rate'], reverse=True)

    vsize = np.array([e['vsize'] for e in entries]).cumsum()
    rate = np.array([e['rate'] for e in entries])

    plt.semilogy(vsize / 1e6, rate, '-')
    plt.xlabel('Mempool size (MB)')
    plt.ylabel('Fee rate (sat/vbyte)')
    plt.grid()
    plt.show()


if __name__ == '__main__':
    main()
