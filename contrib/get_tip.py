#!/usr/bin/env python3
import argparse
import client
import json

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("host")
    parser.add_argument("port", type=int)
    args = parser.parse_args()

    conn = client.Client((args.host, args.port))
    print(conn.call([client.request("blockchain.headers.subscribe")]))

if __name__ == '__main__':
	main()
