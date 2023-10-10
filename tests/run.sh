#!/bin/bash
set -euo pipefail

rm -rf data/
mkdir -p data/{bitcoin,electrum,electrs}

cleanup() {
  trap - SIGTERM SIGINT
  set +eo pipefail
  jobs
  for j in `jobs -rp`
  do
  	kill $j
  	wait $j
  done
}
trap cleanup SIGINT SIGTERM EXIT

BTC="bitcoin-cli -regtest -datadir=data/bitcoin"
ELECTRUM="electrum --regtest"
EL="$ELECTRUM --wallet=data/electrum/wallet"

tail_log() {
	tail -n +0 -F $1 || true
}

echo "Starting $(bitcoind -version | head -n1)..."
bitcoind -regtest -datadir=data/bitcoin -printtoconsole=0 &
BITCOIND_PID=$!

$BTC -rpcwait getblockcount > /dev/null

echo "Creating Electrum `electrum version --offline` wallet..."
WALLET=`$EL --offline create --seed_type=segwit`
MINING_ADDR=`$EL --offline getunusedaddress`

$BTC generatetoaddress 110 $MINING_ADDR > /dev/null
echo `$BTC getblockchaininfo | jq -r '"Generated \(.blocks) regtest blocks (\(.size_on_disk/1e3) kB)"'` to $MINING_ADDR

TIP=`$BTC getbestblockhash`

export RUST_LOG=electrs=debug
electrs \
  --db-dir=data/electrs \
  --daemon-dir=data/bitcoin \
  --network=regtest \
  2> data/electrs/regtest-debug.log &
ELECTRS_PID=$!
tail_log data/electrs/regtest-debug.log | grep -m1 "serving Electrum RPC"
curl localhost:24224 -o metrics.txt

$ELECTRUM daemon --server localhost:60401:t -1 -vDEBUG 2> data/electrum/regtest-debug.log &
ELECTRUM_PID=$!
tail_log data/electrum/regtest-debug.log | grep -m1 "connection established"
$EL getinfo | jq .

echo "Loading Electrum wallet..."
$EL load_wallet

echo "Running integration tests:"

echo " * getbalance"
test "`$EL getbalance | jq -c .`" == '{"confirmed":"550","unmatured":"4950"}'

echo " * getunusedaddress"
NEW_ADDR=`$EL getunusedaddress`

echo " * payto & broadcast"
TXID=$($EL broadcast $($EL payto $NEW_ADDR 123 --fee 0.001 --password=''))

echo " * get_tx_status"
test "`$EL get_tx_status $TXID | jq -c .`" == '{"confirmations":0}'

echo " * getaddresshistory"
test "`$EL getaddresshistory $NEW_ADDR | jq -c .`" == "[{\"fee\":100000,\"height\":0,\"tx_hash\":\"$TXID\"}]"

echo " * getbalance"
test "`$EL getbalance | jq -c .`" == '{"confirmed":"549.999","unmatured":"4950"}'

echo "Generating bitcoin block..."
$BTC generatetoaddress 1 $MINING_ADDR > /dev/null
$BTC getblockcount > /dev/null

echo " * wait for new block"
kill -USR1 $ELECTRS_PID  # notify server to index new block
tail_log data/electrum/regtest-debug.log | grep -m1 "verified $TXID" > /dev/null

echo " * get_tx_status"
test "`$EL get_tx_status $TXID | jq -c .`" == '{"confirmations":1}'

echo " * getaddresshistory"
test "`$EL getaddresshistory $NEW_ADDR | jq -c .`" == "[{\"height\":111,\"tx_hash\":\"$TXID\"}]"

echo " * getbalance"
test "`$EL getbalance | jq -c .`" == '{"confirmed":"599.999","unmatured":"4950.001"}'

echo "Electrum `$EL stop`"  # disconnect wallet
wait $ELECTRUM_PID

kill -INT $ELECTRS_PID  # close server
tail_log data/electrs/regtest-debug.log | grep -m1 "electrs stopped"
wait $ELECTRS_PID

$BTC stop # stop bitcoind
wait $BITCOIND_PID

echo "=== PASSED ==="
