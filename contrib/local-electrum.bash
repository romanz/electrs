#!/bin/bash
set -eux

ADDR=127.0.0.1  # localhost
PORT=50001      # default mainnet Electrum RPC port
PROTOCOL=t      # TCP (no SSL)

# Use only local Electrum server:
electrum --oneserver --server="$ADDR:$PORT:$PROTOCOL" $*
