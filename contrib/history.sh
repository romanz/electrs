#!/bin/bash
set -eu
cd `dirname $0`
.env/bin/python history.py $*
