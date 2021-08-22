#!/bin/bash
set -eux
cd `dirname $0`

cargo fmt --all
cargo build --all --release

NETWORK=$1
shift

DB=./db2
export RUST_LOG=${RUST_LOG-INFO}
target/release/electrs --network $NETWORK --db-dir $DB --daemon-dir $HOME/.bitcoin $*

# use SIGINT to quit
