FROM rust:1.44.1-slim-buster as builder

RUN apt-get update \
  && apt-get install -y --no-install-recommends clang=1:7.* cmake=3.* \
  libsnappy-dev=1.* curl \
  && apt-get clean \
  && rm -rf /var/lib/apt/lists/*

RUN adduser --disabled-login --system --shell /bin/false --uid 1000 user

WORKDIR /home/user
COPY . .
RUN chown -R user .

RUN cargo install --locked --path .

# Create runtime image
FROM debian:buster-slim

RUN adduser --disabled-login --system --shell /bin/false --uid 1000 user

WORKDIR /home/user/app
RUN chown user .
COPY --from=builder /home/user/target/release .

USER user

# Electrum RPC
EXPOSE 50001

# Prometheus monitoring
EXPOSE 4224

STOPSIGNAL SIGINT

HEALTHCHECK CMD curl -fSs http://localhost:4224/ || exit 1

ENTRYPOINT ["./electrs", "-vvvv", "--timestamp"]
