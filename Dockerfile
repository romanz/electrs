FROM debian:buster-slim

RUN apt-get update
RUN apt-get install -y clang cmake
RUN apt-get install -y libsnappy-dev
RUN apt-get install -y curl

RUN adduser --disabled-login --system --shell /bin/false --uid 1000 user

ARG RUST_VERSION=1.34.0
ENV RUSTUP_HOME /usr/local/rustup
ENV CARGO_HOME /usr/local/cargo
ENV PATH $CARGO_HOME/bin:$PATH

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- \
  -y \
  --verbose \
  --profile minimal \
  --default-toolchain $RUST_VERSION

RUN chmod -R a+w $RUSTUP_HOME $CARGO_HOME

USER user
WORKDIR /home/user
COPY ./ /home/user

RUN cargo build --release
RUN cargo install --path .

# Electrum RPC
EXPOSE 50001

# Prometheus monitoring
EXPOSE 4224

STOPSIGNAL SIGINT
