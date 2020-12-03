#!/bin/bash
set -euo pipefail

rm -rf data/
mkdir -p data/{bitcoin,electrum,electrs}

cleanup() {
  trap - SIGTERM SIGINT
  set +eo pipefail
  kill `jobs -rp`
  wait `jobs -rp`
}
trap cleanup SIGINT SIGTERM EXIT

BTC="bitcoin-cli -regtest -datadir=data/bitcoin"
ELECTRUM="electrum --regtest"
EL="$ELECTRUM --wallet=data/electrum/wallet"

tail_log() {
	tail -n +0 -F data/$1 || true
}

echo "Starting $(bitcoind -version | head -n1)..."
bitcoind -regtest -datadir=data/bitcoin -printtoconsole=0 &

$BTC -rpcwait getblockcount > /dev/null

echo "Creating Electrum `electrum version --offline` wallet..."
WALLET=`$EL --offline create --seed_type=segwit`
MINING_ADDR=`$EL --offline getunusedaddress`

$BTC generatetoaddress 110 $MINING_ADDR > /dev/null
echo `$BTC getblockchaininfo | jq -r '"Generated \(.blocks) regtest blocks (\(.size_on_disk/1e3) kB)"'` to $MINING_ADDR

TIP=`$BTC getbestblockhash`
$BTC getblocklocations $TIP 1000 > /dev/null  # make sure the new RPC works

export RUST_LOG=electrs=debug
electrs --db-dir=data/electrs --daemon-dir=data/bitcoin --network=regtest 2> data/electrs/regtest-debug.log &
tail_log electrs/regtest-debug.log | grep -m1 "serving Electrum RPC"
tail_log electrs/regtest-debug.log | grep -m1 "verified 111 blocks"  # TODO: wait on subscription

echo -e '{"jsonrpc": "2.0", "method": "server.version", "params": ["test", "1.4"], "id": 0}\n{"jsonrpc": "2.0", "method": "blockchain.headers.subscribe", "id": 1}' \
| netcat localhost 60401 > data/electrs/regtest-sub.log &

ELECTRS_VERSION=`tail_log electrs/regtest-sub.log | head -n1 | jq -C '.result[0]'`
TIP_HEADER=`tail_log electrs/regtest-sub.log | grep -m1 '"height":' | jq .result`

echo "Started ${ELECTRS_VERSION/\// } with `jq -r .height <<< "$TIP_HEADER"` blocks indexed"
test `jq -r .hex <<< "$TIP_HEADER"` == `$BTC getblockheader $TIP false`

$ELECTRUM daemon --server localhost:60401:t -1 -vDEBUG 2> data/electrum/regtest-debug.log &
tail_log electrum/regtest-debug.log | grep -m1 "connection established"
$EL getinfo

echo "Loading Electrum wallet..."
test `$EL load_wallet` == "true"

echo "Running integration tests:"

echo " * getbalance"
test "`$EL getbalance | jq -c .`" == '{"confirmed":"550","unmatured":"4950"}'

echo " * getunusedaddress"
NEW_ADDR=`$EL getunusedaddress`

echo " * payto & broadcast"
TXID=$($EL broadcast $($EL payto $NEW_ADDR 123 --fee 0.001))

echo " * get_tx_status"
test "`$EL get_tx_status $TXID | jq -c .`" == '{"confirmations":0}'

echo " * getaddresshistory"
test "`$EL getaddresshistory $NEW_ADDR | jq -c .`" == "[{\"fee\":100000,\"height\":0,\"tx_hash\":\"$TXID\"}]"

echo " * getbalance"
test "`$EL getbalance | jq -c .`" == '{"confirmed":"550","unconfirmed":"-0.001","unmatured":"4950"}'

echo "Generating bitcoin block..."
$BTC generatetoaddress 1 $MINING_ADDR > /dev/null
$BTC getblockcount > /dev/null
tail_log electrum/regtest-debug.log | grep -m1 "verified $TXID"

echo " * get_tx_status"
test "`$EL get_tx_status $TXID | jq -c .`" == '{"confirmations":1}'

echo " * getaddresshistory"
test "`$EL getaddresshistory $NEW_ADDR | jq -c .`" == "[{\"fee\":null,\"height\":111,\"tx_hash\":\"$TXID\"}]"

echo " * getbalance"
test "`$EL getbalance | jq -c .`" == '{"confirmed":"599.999","unmatured":"4950.001"}'

$BTC stop  # should also stop electrs
echo "Electrum `$EL stop`"

echo "=== PASSED ==="
