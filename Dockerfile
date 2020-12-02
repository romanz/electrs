FROM debian:buster-slim as updated
RUN apt-get update
# Install Bitcoin Core runtime dependencies
RUN apt-get install -qqy libevent-dev libboost-system-dev libboost-filesystem-dev libboost-test-dev libboost-thread-dev
# Install a few test tools
RUN apt-get install -qqy jq netcat

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

### Electrum Rust Server ###
FROM updated as electrs-build
# Install Rust 1.41.1
RUN apt-get install -qq -y clang cmake cargo rustc

# Build, test and install electrs
WORKDIR /build/electrs
COPY . .
RUN cargo build --locked --release --all
RUN cargo test --locked --release --all

FROM updated as result
### Electrum ###
# Download latest Electrum wallet
RUN apt-get install -qqy wget libsecp256k1-0 python3-cryptography
ARG ELECTRUM_VERSION=4.0.5
WORKDIR /build/electrum
RUN wget -q https://download.electrum.org/$ELECTRUM_VERSION/Electrum-$ELECTRUM_VERSION.tar.gz \
&& (echo "6790407e21366186d928c8e653e3ab38476ca86e4797aa4db94dcca2384db41a Electrum-$ELECTRUM_VERSION.tar.gz" | sha256sum -c -)
# Unpack Electrum and install it
RUN tar xfz Electrum-$ELECTRUM_VERSION.tar.gz && ln -s $PWD/Electrum-$ELECTRUM_VERSION/run_electrum /usr/bin/electrum
RUN electrum version --offline

# Copy the binaries
RUN apt-get install -qqy jq netcat
COPY --from=electrs-build /build/electrs/target/release/electrs_rpc /usr/bin/electrs
COPY --from=bitcoin-build /build/bitcoin/src/bitcoind /build/bitcoin/src/bitcoin-cli /usr/bin/
RUN bitcoind -version && bitcoin-cli -version
WORKDIR /
