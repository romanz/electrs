### Electrum Rust Server ###
FROM rust:1.41.1-slim as electrs-build
RUN apt-get update
RUN apt-get install -qq -y clang cmake
RUN rustup component add rustfmt

# Build, test and install electrs
WORKDIR /build/electrs
COPY . .
RUN cargo fmt -- --check
RUN cargo build --locked --release --all
RUN cargo test --locked --release --all
RUN cargo install --locked --path electrs_rpc

FROM debian:buster-slim as updated
RUN apt-get update
# Install Bitcoin Core runtime dependencies
RUN apt-get install -qqy libevent-dev libboost-system-dev libboost-filesystem-dev libboost-test-dev libboost-thread-dev

### Bitcoin Core ###
FROM updated as bitcoin-build
# Buildtime dependencies
RUN apt-get install -qqy build-essential libtool autotools-dev automake pkg-config bsdmainutils python3
# Clone source
RUN apt-get install -qqy git
WORKDIR /build/bitcoin
RUN git clone --branch locations https://github.com/romanz/bitcoin.git .

# Build bitcoin
RUN ./autogen.sh
RUN ./configure --disable-tests --disable-wallet --disable-bench --without-gui --without-miniupnpc
RUN make -j"$(($(nproc)+1))"

FROM updated as result
# Copy the binaries
COPY --from=electrs-build /usr/local/cargo/bin/electrs_rpc /usr/bin/electrs
COPY --from=bitcoin-build /build/bitcoin/src/bitcoind /build/bitcoin/src/bitcoin-cli /usr/bin/
RUN bitcoind -version && bitcoin-cli -version

### Electrum ###
# Clone latest Electrum wallet and a few test tools
WORKDIR /build/
RUN apt-get install -qqy git libsecp256k1-0 python3-cryptography python3-setuptools python3-pip jq netcat
RUN git clone --recurse-submodules https://github.com/spesmilo/electrum/ && cd electrum/ && git log -1
RUN python3 -m pip install -e electrum/

RUN electrum version --offline
WORKDIR /
