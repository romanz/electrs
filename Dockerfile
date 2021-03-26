### Electrum Rust Server ###
FROM rust:1.48.0-slim as electrs-build
RUN apt-get update
RUN apt-get install -qq -y clang cmake
RUN rustup component add rustfmt

# Build, test and install electrs
WORKDIR /build/electrs
COPY . .
RUN cargo fmt -- --check
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
RUN wget -q https://bitcoincore.org/bin/bitcoin-core-0.21.0/bitcoin-0.21.0-x86_64-linux-gnu.tar.gz
RUN tar xvf bitcoin-0.21.0-x86_64-linux-gnu.tar.gz
RUN mv -v bitcoin-0.21.0/bin/bitcoind .
RUN mv -v bitcoin-0.21.0/bin/bitcoin-cli .

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
