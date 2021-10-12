#!/bin/bash
set -eu

cd -- "$( dirname -- "${BASH_SOURCE[0]}" )"

venv=false
if [ "$1" = "--venv" ];
then
    shift
    venv=true
fi

if [ $venv = true ] && [ ! -d .venv ]; then
    virtualenv .venv
    .venv/bin/pip install pycoin logbook prettytable
fi

exec .venv/bin/python history.py "$@"
