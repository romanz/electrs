import json
import socket

class Client:
    def __init__(self, addr):
        self.s = socket.create_connection(addr)
        self.f = self.s.makefile('r')
        self.id = 0

    def request(self, method, *args):
        self.id += 1
        return {
            'id': self.id,
            'method': method,
            'params': list(args),
            'jsonrpc': '2.0',
        }

    def call(self, *requests):
        msg = json.dumps(requests) + '\n'
        self.s.sendall(msg.encode('ascii'))
        return json.loads(self.f.readline())
