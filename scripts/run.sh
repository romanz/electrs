#!/bin/bash
set -eu
trap 'kill $(jobs -p)' EXIT

DELAY=5
LOG=/tmp/electrs.log
CARGO="cargo +stable"

tail -v -n0 -F "$LOG" &

export RUST_BACKTRACE=1
while :
do
	$CARGO fmt
	$CARGO check --release
	$CARGO run --release -- $* 2>> "$LOG"
	echo "Restarting in $DELAY seconds..."
	sleep $DELAY
done
