#!/bin/bash
set -eux
cd `dirname $0`

cargo fmt
cargo build --all --release

NETWORK=$1
shift

DB=./db1  # $HOME/tmp/electrs_db/mainnet_zstd
QUERY="target/release/electrs_rpc --network $NETWORK --db-dir $DB --daemon-dir $HOME/.bitcoin"
export RUST_LOG=${RUST_LOG-info}
$QUERY $*

# use SIGINT to quit
