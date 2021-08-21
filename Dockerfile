### Electrum Rust Server ###
FROM rust:1.41.1-slim as electrs-build
RUN apt-get update
RUN apt-get install -qq -y clang cmake
RUN rustup component add rustfmt clippy

# Build, test and install electrs
WORKDIR /build/electrs
COPY . .
RUN cargo fmt -- --check
RUN cargo clippy
RUN cargo build --locked --release --all
RUN cargo test --locked --release --all
RUN cargo install --locked --path .

FROM debian:buster-slim as updated
RUN apt-get update -qqy

### Bitcoin Core ###
FROM updated as bitcoin-build
# Download
RUN apt-get install -qqy wget
WORKDIR /build/bitcoin
ARG BITCOIND_VERSION=0.21.1
RUN wget -q https://bitcoincore.org/bin/bitcoin-core-$BITCOIND_VERSION/bitcoin-$BITCOIND_VERSION-x86_64-linux-gnu.tar.gz
RUN tar xvf bitcoin-$BITCOIND_VERSION-x86_64-linux-gnu.tar.gz
RUN mv -v bitcoin-$BITCOIND_VERSION/bin/bitcoind .
RUN mv -v bitcoin-$BITCOIND_VERSION/bin/bitcoin-cli .

FROM updated as result
# Copy the binaries
COPY --from=electrs-build /usr/local/cargo/bin/electrs /usr/bin/electrs
COPY --from=bitcoin-build /build/bitcoin/bitcoind /build/bitcoin/bitcoin-cli /usr/bin/
RUN bitcoind -version && bitcoin-cli -version

### Electrum ###
# Clone latest Electrum wallet and a few test tools
WORKDIR /build/
RUN apt-get install -qqy git libsecp256k1-0 python3-cryptography python3-setuptools python3-pip jq
RUN git clone --recurse-submodules https://github.com/spesmilo/electrum/ && cd electrum/ && git log -1
RUN python3 -m pip install -e electrum/

RUN electrum version --offline
WORKDIR /
