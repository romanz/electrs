import binascii
import json
import os
import socket


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
            assert reply['error'] is None, reply
            assert reply['id'] == self.index

        self.index += 1
        return [d['result'] for d in replies]
