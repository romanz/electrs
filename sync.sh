#!/bin/bash
set -eux
cd `dirname $0`

cargo fmt --all
cargo build --all --release

NETWORK=$1
shift

CMD="target/release/sync --network $NETWORK --db-dir ./db2 --daemon-dir $HOME/.bitcoin"
export RUST_LOG=${RUST_LOG-info}
$CMD $*

# use SIGINT to quit
