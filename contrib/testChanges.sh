#!/bin/bash
cd `dirname $0`/..
cargo build --locked --no-default-features --all
cargo build --locked --no-default-features --all
cargo build --locked --all
cargo build --locked --features metrics_process --all
cargo fmt
cargo clippy -- -D warnings
cargo test --locked --all
