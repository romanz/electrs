#!/bin/bash

set -e

cd -- "$( dirname -- "${BASH_SOURCE[0]}" )"

cmd="$(basename -- "${BASH_SOURCE[0]}" .sh)".py

if [ "$1" = "--venv" ]; then
    shift
    if [ ! -d .venv ]; then
        virtualenv .venv
        .venv/bin/pip install pycoin logbook prettytable base58
    fi
    PATH=$PWD/.venv/bin:$PATH
fi

exec python "$cmd" "$@"
