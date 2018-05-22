import binascii
import json
import socket
import time

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
    t = time.time()
    txids, = request('getrawmempool', [[False]])
    print('{:.3f} {}'.format(time.time() - t, len(txids)))
    t = time.time()
    txids = list(map(lambda a: [a], txids))
    entries = request('getmempoolentry', txids)
    print('{:.3f} {}'.format(time.time() - t, len(entries)))


if __name__ == '__main__':
    main()
