import json
import socket

class Client:
    def __init__(self, addr):
        self.s = socket.create_connection(addr)
        self.f = self.s.makefile('r')
        self.id = 0

    def call(self, requests):
        requests = list(requests)
        for request in requests:
            request['id'] = self.id
            request['jsonrpc'] = '2.0'
            self.id += 1

        msg = json.dumps(requests) + '\n'
        self.s.sendall(msg.encode('ascii'))
        response = json.loads(self.f.readline())
        try:
            return [r['result'] for r in response]
        except KeyError:
            raise ValueError(response)


def request(method, *args):
    return {'method': method, 'params': list(args)}
