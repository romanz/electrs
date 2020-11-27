FROM debian:buster-slim
RUN apt-get update

### Bitcoin ###

# Buildtime dependencies
RUN apt-get install -qqy build-essential libtool autotools-dev automake pkg-config bsdmainutils python3

# Runtime dependencies
RUN apt-get install -qqy libevent-dev libboost-system-dev libboost-filesystem-dev libboost-test-dev libboost-thread-dev

# Clone source
RUN apt-get install -qqy git
WORKDIR /build/bitcoin
RUN git clone --branch locations https://github.com/romanz/bitcoin.git .

# Build
RUN ./autogen.sh && ./configure --disable-wallet --without-gui --without-miniupnpc && make -j"$(($(nproc)+1))"

# Install
RUN mv -v src/bitcoind src/bitcoin-cli /usr/bin/

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

### Electrum Rust Server ###

# Install Rust 1.41.1
RUN apt-get install -qq -y clang cmake cargo rustc

# Build electrs and install it
WORKDIR /build/electrs
COPY . .
RUN cargo build --locked --release -p electrs_rpc
RUN cp target/release/electrs_rpc /usr/bin/electrs

# Install a few test tools and copy the integration tests
RUN apt-get install -qqy jq netcat

