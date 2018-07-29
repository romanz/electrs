#!/bin/bash
set -eu

T=5
export RUST_BACKTRACE=1
while :
do
	cargo fmt
	cargo check --release
	cargo run --release -- $*
	echo "Restarting in $T seconds..."
	sleep $T
done
