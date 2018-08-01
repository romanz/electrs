#!/bin/bash
set -eu
trap 'kill $(jobs -p)' EXIT

DELAY=5
LOG=/tmp/electrs.log
tail -v -n0 -F "$LOG" &

export RUST_BACKTRACE=1
while :
do
	cargo fmt
	cargo check --release
	cargo run --release -- $* 2>> "$LOG"
	echo "Restarting in $DELAY seconds..."
	sleep $DELAY
done
