#!/bin/bash

cd `dirname $0`/../.git/hooks/
ln -s ../../.hooks/pre-commit
