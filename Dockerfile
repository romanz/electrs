FROM rust:latest

RUN apt-get update
RUN apt-get install -y clang cmake

RUN cargo install electrs

CMD ["electrs", "-vvvv", "--timestamp"]
