import struct
import zmq

def main():
    ctx = zmq.Context()
    sub = ctx.socket(zmq.SUB)
    sub.setsockopt_string(zmq.SUBSCRIBE, 'hashblock')
    sub.setsockopt_string(zmq.SUBSCRIBE, 'hashtx')
    sub.connect('tcp://localhost:28332')

    while True:
        topic, msg, count = sub.recv_multipart()
        topic = topic.decode('ascii')
        count, = struct.unpack('<L', count)
        msg = msg.hex()
        print('{} {}: {}'.format(topic, msg, count))

if __name__ == '__main__':
    main()
