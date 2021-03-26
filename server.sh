#!/bin/bash
set -eux
cd `dirname $0`

cargo fmt --all
cargo build --all --release

NETWORK=$1
shift

DB=./db2  # $HOME/tmp/electrs_db/mainnet_zstd
CMD="target/release/electrs --network $NETWORK --db-dir $DB --daemon-dir $HOME/.bitcoin"
export RUST_LOG=${RUST_LOG-info}
$CMD $*

# use SIGINT to quit
