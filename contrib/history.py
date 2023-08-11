#!/usr/bin/env python3
import argparse
import datetime
import hashlib
import io
import sys

from logbook import Logger, StreamHandler
import prettytable

import client

log = Logger('electrum')


def _script_hash(script):
    return hashlib.sha256(script).digest()[::-1].hex()


def show_rows(rows, field_names):
    t = prettytable.PrettyTable()
    t.field_names = field_names
    t.add_rows(rows)
    for f in t.field_names:
        if "mBTC" in f:
            t.align[f] = "r"
    print(t)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--host', default='localhost')
    parser.add_argument('--network', default='mainnet')
    parser.add_argument('address', nargs='+')
    parser.add_argument('--only-subscribe', action='store_true', default=False)
    parser.add_argument('--no-merkle-proofs', action='store_true', default=False)
    args = parser.parse_args()

    if args.network == 'regtest':
        port = 60401
        from pycoin.symbols.xrt import network
    elif args.network == 'testnet':
        port = 60001
        from pycoin.symbols.xtn import network
    elif args.network == 'mainnet':
        port = 50001
        from pycoin.symbols.btc import network
    else:
        raise ValueError(f"unknown network: {args.network}")

    hostport = (args.host, port)
    log.info('connecting to {}:{}', *hostport)
    conn = client.Client(hostport)

    tip, = conn.call([client.request('blockchain.headers.subscribe')])

    script_hashes = [
        _script_hash(network.parse.address(addr).script())
        for addr in args.address
    ]

    conn.call(
        client.request('blockchain.scripthash.subscribe', script_hash)
        for script_hash in script_hashes
    )
    log.info('subscribed to {} scripthashes', len(script_hashes))
    if args.only_subscribe:
        return

    balances = conn.call(
        client.request('blockchain.scripthash.get_balance', script_hash)
        for script_hash in script_hashes
    )

    unspents = conn.call(
        client.request('blockchain.scripthash.listunspent', script_hash)
        for script_hash in script_hashes
    )
    for addr, balance, unspent in sorted(zip(args.address, balances, unspents), key=lambda v: v[0]):
        if unspent:
            log.debug("{}: confirmed={:,.5f} mBTC, unconfirmed={:,.5f} mBTC",
                addr, balance["confirmed"] / 1e5, balance["unconfirmed"] / 1e5)
            for u in unspent:
                log.debug("\t{}:{} = {:,.5f} mBTC {}",
                    u["tx_hash"], u["tx_pos"], u["value"] / 1e5,
                    f'@ {u["height"]}' if u["height"] else "")

    histories = conn.call(
        client.request('blockchain.scripthash.get_history', script_hash)
        for script_hash in script_hashes
    )
    txids_map = dict(
        (tx['tx_hash'], tx['height'] if tx['height'] > 0 else None) 
        for history in histories 
        for tx in history
    )
    log.info('got history of {} transactions', len(txids_map))

    txs = map(network.tx.from_hex, conn.call(
        client.request('blockchain.transaction.get', txid)
        for txid in txids_map.keys()
    ))
    txs_map = dict(zip(txids_map.keys(), txs))
    log.info('loaded {} transactions', len(txids_map))

    confirmed_txids = {txid: height for txid, height in txids_map.items() if height is not None}

    heights = set(confirmed_txids.values())
    def _parse_header(header):
        return network.block.parse_as_header(io.BytesIO(bytes.fromhex(header)))
    headers = map(_parse_header, conn.call(
        client.request('blockchain.block.header', height)
        for height in heights
    ))
    def _parse_timestamp(header):
        return datetime.datetime.utcfromtimestamp(header.timestamp).strftime('%Y-%m-%dT%H:%M:%SZ')
    timestamps = map(_parse_timestamp, headers)
    timestamps_map = dict(zip(heights, timestamps))
    log.info('loaded {} header timestamps', len(heights))

    if args.no_merkle_proofs:
        return

    proofs = conn.call(
        client.request('blockchain.transaction.get_merkle', txid, height)
        for txid, height in confirmed_txids.items()
    )
    log.info('loaded {} merkle proofs', len(proofs))  # TODO: verify proofs

    sorted_txdata = sorted(
        (proof['block_height'], proof['pos'], txid) 
        for proof, txid in zip(proofs, confirmed_txids)
    )

    utxos = {}
    balance = 0

    rows = []
    script_hashes = set(script_hashes)
    for block_height, block_pos, txid in sorted_txdata:
        tx_obj = txs_map[txid]
        for txi in tx_obj.txs_in:
            utxos.pop((str(txi.previous_hash), txi.previous_index), None)

        for index, txo in enumerate(tx_obj.txs_out):
            if _script_hash(txo.puzzle_script()) in script_hashes:
                utxos[(txid, index)] = txo
    
        diff = sum(txo.coin_value for txo in utxos.values()) - balance
        balance += diff
        confirmations = tip['height'] - block_height + 1
        rows.append([txid, timestamps_map[block_height], block_height, confirmations, f'{diff/1e5:,.5f}', f'{balance/1e5:,.5f}'])
    show_rows(rows, ["txid", "block timestamp", "height", "confirmations", "delta (mBTC)", "total (mBTC)"])

    tip_header = _parse_header(tip['hex'])
    log.info('tip={}, height={} @ {}', tip_header.id(), tip['height'], _parse_timestamp(tip_header))

    unconfirmed = {txs_map[txid] for txid, height in txids_map.items() if height is None}
    # TODO: show unconfirmed balance

if __name__ == '__main__':
    StreamHandler(sys.stderr).push_application()
    main()
