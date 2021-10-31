## Manual installation from source

**See below for automated/binary installation options.**

### Build dependencies

Note for Raspberry Pi 4 owners: the old versions of OS/toolchains produce broken binaries.
Make sure to use latest OS! (see #226)

Install [recent Rust](https://rustup.rs/) (1.48.0+, `apt install cargo` is preferred for Debian 11),
[latest Bitcoin Core](https://bitcoincore.org/en/download/) (0.21+)
and [latest Electrum wallet](https://electrum.org/#download) (4.0+).

Also, install the following packages (on Debian or Ubuntu):
```bash
$ sudo apt update
$ sudo apt install clang cmake build-essential  # for building 'rust-rocksdb'
```

There are two ways to compile `electrs`: by statically linking to `librocksdb` or dynamically linking.

The advantages of static linking:

* The binary is self-contained and doesn't need other dependencies, it can be transferred to other machine without worrying
* The binary should work pretty much with every common distro
* Different library installed elsewhere doesn't affect the behavior of `electrs`

The advantages of dynamic linking:

* If a (security) bug is found in the library, you only need to upgrade/recompile the library to fix it, no need to recompile `electrs`
* Updating rocksdb can be as simple as `apt upgrade`
* The build is significantly faster (if you already have the binary version of the library from packages)
* The build is deterministic
* Cross compilation is more reliable
* If another application is also using `rocksdb`, you don't store it on disk and in RAM twice

If you decided to use dynamic linking, you will also need to install the library ([6.11.4 release](https://github.com/facebook/rocksdb/releases/tag/v6.11.4) is required).
On [Debian 11 (bullseye)](https://packages.debian.org/bullseye/librocksdb-dev) and [Ubuntu 21.04 (hirsute)](https://packages.ubuntu.com/hirsute/librocksdb-dev):

```bash
$ sudo apt install librocksdb-dev=6.11.4-3
```

#### Preparing for cross compilation

Cross compilation can save you some time since you can compile `electrs` for a slower computer (like Raspberry Pi) on a faster machine
even with different CPU architecture.
Skip this if it's not your case.

If you want to cross-compile, you need to install some additional packages.
These cross compilation instructions use `aarch64`/`arm64` + Linux as an example.
(The resulting binary should work on RPi 4 with aarch64-enabled OS).
Change to your desired architecture/OS.

If you use Debian (or a derived distribution) you need to enable the target architecture:

```
$ sudo dpkg --add-architecture arm64
$ sudo apt update
```

If you use `cargo` from the repository

```bash
$ sudo apt install gcc-aarch64-linux-gnu gcc-aarch64-linux-gnu libc6-dev:arm64 libstd-rust-dev:arm64
```

If you use Rustup:

```bash
$ sudo apt install gcc-aarch64-linux-gnu gcc-aarch64-linux-gnu libc6-dev:arm64
$ rustup target add aarch64-unknown-linux-gnu
```

If you decided to use the system rocksdb (recommended if the target OS supports it), you need the version from the other architecture:

```bash
$ sudo apt install librocksdb-dev:arm64
```

#### Preparing man page generation (optional)

Optionally, you may install [`cfg_me`](https://github.com/Kixunil/cfg_me) tool for generating the manual page.
The easiest way is to run `cargo install cfg_me`.

#### Download electrs

```bash
$ git clone https://github.com/romanz/electrs
$ cd electrs
```

### Build

Note: you need to have enough free RAM to build `electrs`.
The build will fail otherwise.
Close those 100 old tabs in the browser. ;)

#### Cargo features

By default `electrs` builds with Prometheus support.
However this causes problems on some platforms.
If you don't need Prometheus you may disable it using `--no-default-features` argument to `cargo build`/`cargo install`.

#### Static linking

First build should take ~20 minutes:
```bash
$ cargo build --locked --release
```

If RocksDB build fails with "`undefined reference to __atomic_*`" linker errors
(usually happens on a 32-bit OS), set the following environment variable:
```bash
$ RUSTFLAGS="-C link-args=-latomic" cargo build --locked --release
```
Relevant issues: [#134](https://github.com/romanz/electrs/issues/134) and [#391](https://github.com/romanz/electrs/issues/391).

#### Dynamic linking

```
$ ROCKSDB_INCLUDE_DIR=/usr/include ROCKSDB_LIB_DIR=/usr/lib cargo build --locked --release
```

#### Cross compilation

Run one of the commands above (depending on linking type) with argument `--target aarch64-unknown-linux-gnu` and prepended with env vars: `BINDGEN_EXTRA_CLANG_ARGS="-target gcc-aarch64-linux-gnu" RUSTFLAGS="-C linker=aarch64-linux-gnu-gcc"`

E.g. for dynamic linking case:

```
$ ROCKSDB_INCLUDE_DIR=/usr/include ROCKSDB_LIB_DIR=/usr/lib BINDGEN_EXTRA_CLANG_ARGS="-target gcc-aarch64-linux-gnu" RUSTFLAGS="-C linker=aarch64-linux-gnu-gcc" cargo build --locked --release --target aarch64-unknown-linux-gnu
```

It's a bit long but sufficient! You will find the resulting binary in `target/aarch64-unknown-linux-gnu/release/electrs` - copy it to your target machine.

#### Generating man pages

If you installed `cfg_me` to generate man page, you can run `cfg_me man` to see it right away or `cfg_me -o electrs.1 man` to save it into a file (`electrs.1`).

## Docker-based installation from source

**Important**: The `Dockerfile` is provided for demonstration purposes and may NOT be suitable for production use.
The maintainers of electrs are not deeply familiar with Docker, so you should DYOR.
If you are not familiar with Docker either it's probably be safer to NOT use it.

Note: currently Docker installation links statically

Note: health check only works if Prometheus is running on port 4224 inside container

```bash
$ docker build -t electrs-app .
$ mkdir db
$ docker run --network host \
             --volume $HOME/.bitcoin:/home/user/.bitcoin:ro \
             --volume $PWD/db:/home/user/db \
             --env ELECTRS_DB_DIR=/home/user/db \
             --rm -i -t electrs-app
```

If not using the host-network, you probably want to expose the ports for electrs and Prometheus like so:

```bash
$ docker run --volume $HOME/.bitcoin:/home/user/.bitcoin:ro \
             --volume $PWD/db:/home/user/db \
             --env ELECTRS_DB_DIR=/home/user/db \
             --env ELECTRS_ELECTRUM_RPC_ADDR=0.0.0.0:50001 \
             --env ELECTRS_MONITORING_ADDR=0.0.0.0:4224 \
             --rm -i -t electrs-app
```

To access the server from outside Docker, add `-p 50001:50001 -p 4224:4224` but be aware of the security risks. Good practice is to group containers that needs access to the server inside the same Docker network and not expose the ports to the outside world.
