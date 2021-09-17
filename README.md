# Electrum Server in Rust

[![workflows](https://github.com/romanz/electrs/workflows/Rust/badge.svg)](https://github.com/romanz/electrs/actions)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg?style=flat-square)](http://makeapullrequest.com)
[![crates.io](https://img.shields.io/crates/v/electrs.svg)](https://crates.io/crates/electrs)
[![gitter.im](https://badges.gitter.im/romanz/electrs.svg)](https://gitter.im/romanz/electrs)

An efficient re-implementation of Electrum Server, inspired by [ElectrumX](https://github.com/kyuupichan/electrumx), [Electrum Personal Server](https://github.com/chris-belcher/electrum-personal-server) and [bitcoincore-indexd](https://github.com/jonasschnelli/bitcoincore-indexd).

The motivation behind this project is to enable a user to run his own Electrum server,
with required hardware resources not much beyond those of a [full node](https://en.bitcoin.it/wiki/Full_node#Why_should_you_use_a_full_node_wallet).
The server indexes the entire Bitcoin blockchain, and the resulting index enables fast queries for any given user wallet,
allowing the user to keep real-time track of his balances and his transaction history using the [Electrum wallet](https://electrum.org/).
Since it runs on the user's own machine, there is no need for the wallet to communicate with external Electrum servers,
thus preserving the privacy of the user's addresses and balances.


## Usage

**Please prefer to use OUR usage guide!**

External guides such as RaspiBolt can be out-of-date and have various problems.
At least double-check that the guide you're using is actively maintained.
If you can't use our guide ask about what you don't understand or consider using automated deployments.

See [here](doc/usage.md) for installation, build and usage instructions.

## Features

 * Supports Electrum protocol [v1.4](https://electrumx-spesmilo.readthedocs.io/en/latest/protocol.html)
 * Maintains an index over transaction inputs and outputs, allowing fast balance queries
 * Fast synchronization of the Bitcoin blockchain (~4 hours for ~336GB @ August 2021) using HDD storage.
 * Low index storage overhead (~10%), relying on a local full node for transaction retrieval
 * Efficient mempool tracker (allowing better fee [estimation](https://github.com/spesmilo/electrum/blob/59c1d03f018026ac301c4e74facfc64da8ae4708/RELEASE-NOTES#L34-L46))
 * Low CPU & memory usage (after initial indexing)
 * [`txindex`](https://github.com/bitcoinbook/bitcoinbook/blob/develop/ch03.asciidoc#txindex) is not required for the Bitcoin node
 * Uses a single [RocksDB](https://github.com/spacejam/rust-rocksdb) database, for better consistency and crash recovery

## Index database

The database schema is described [here](doc/schema.md).
