#!/bin/bash

set -eo pipefail
shopt -s expand_aliases 

# A simple script for checking HTTP API stability by comparing the responses
# between two running electrs instances connected to a regtest node backend.

: ${NODE_DIR?missing NODE_DIR} # for bitcoind/elementds
: ${ELECTRS1_URL?missing ELECTRS1_URL}
: ${ELECTRS2_URL?missing ELECTRS2_URL}
# Set ELEMENTS_CHAIN for Elements-based chains (e.g. to 'elementsregtest')

alias cli="$([[ -z $ELEMENTS_CHAIN ]] && echo bitcoin-cli -regtest || echo elements-cli -chain=$ELEMENTS_CHAIN) -datadir=$NODE_DIR"

check() {
    echo "Checking GET $1 ..."
    local res1=$(curl -f -s "$ELECTRS1_URL$1" || echo "Request to ELECTRS1 failed")
    local res2=$(curl -f -s "$ELECTRS2_URL$1" || echo "Request to ELECTRS2 failed")
    { if [[ "$res1" = "{"* || "$res1" = "["* ]]; then
        # Use `jq` for canonicalized ordering and to display a diff of beautified JSON
        local sort_arr='walk(if type == "array" then sort else . end)'
        diff -u1 <(jq --sort-keys "$sort_arr" <<< $res1) <(jq --sort-keys "$sort_arr" <<< $res2)
    else
        diff -u1 <(echo "$res1") <(echo "$res2")
    fi } && echo OK || echo No match
}

sync() { pkill -USR1 electrs; sleep 1; }

# Ensure both electrs instances are connected to the same node backend
check /blocks/tip/hash

# Send an unconfirmed transaction
address=$(cli getnewaddress)
txid=$(cli sendtoaddress $address 1.234)
sync
check /address/$address
check /address/$address/txs
check /address/$address/utxo
check /tx/$txid
check /mempool
check /mempool/recent

# Mine a block confirming the transaction
blockhash=$(cli -generate 1 | jq -r .blocks[0])
sync
check /block/$blockhash
check /block/$blockhash/txs
check /blocks
check /address/$address
check /address/$address/txs
check /address/$address/utxo
check /tx/$txid

# Elements-only tests
if [[ -n $ELEMENTS_CHAIN ]]; then
    # Test non-confidential transaction
    uc_address=$(cli getaddressinfo $address | jq -r .unconfidential)
    uc_txid=$(cli sendtoaddress $uc_address 5.678)
    sync 
    check /address/$uc_address
    check /tx/$uc_txid

    # Test asset issuance (blinded w/o contract hash & unblinded w/ contract hash)
    asset1=$(cli issueasset 10 20 true)
    asset2=$(cli issueasset 30 40 false 3333333333333333333333333333333333333333333333333333333333333333)
    sync 
    check_asset() {
        check /asset/$(jq -r .asset <<< $1)
        check /tx/$(jq -r .txid <<< $1) # issuance tx
    }
    check_asset "$asset1"
    check_asset "$asset2"
    cli -generate 1 > /dev/null
    sync 
    check_asset "$asset1"
    check_asset "$asset2"

    # Test transactions transferring an issued asset (confidential & non-confidential)
    asset1_id=$(jq -r .asset <<< $asset1)
    asset2_id=$(jq -r .asset <<< $asset2)
    i_txid=$(cli    -named sendtoaddress address=$address    amount=0.987 assetlabel=$asset1_id)
    i_uc_txid=$(cli -named sendtoaddress address=$uc_address amount=0.654 assetlabel=$asset2_id)
    sync 
    check /tx/$i_txid
    check /tx/$i_uc_txid
    check /address/$uc_address
    check /address/$uc_address/utxo

    # Test issuance with no reissuance tokens
    check_asset "$(cli issueasset 10 0 false && sync)"
fi