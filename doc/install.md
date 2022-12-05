## Quickstart

<details>
<summary>Building from source on an Ubuntu 21.10 VM:</summary>

```bash
$ sudo apt update
$ sudo apt install -y clang cmake build-essential git cargo 
$ git clone https://github.com/romanz/electrs 
$ cd electrs
$ cargo build --locked --release
$ ./target/release/electrs --version  # should print the latest version
```

</details>

[![asciicast](https://asciinema.org/a/XKznxilP4O7lCZiVZ9vZNd5vx.svg)](https://asciinema.org/a/XKznxilP4O7lCZiVZ9vZNd5vx?speed=3)

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

For other versions of Debian or Ubuntu, you can build librocksdb and install inside `/usr/local` directory using following command.

```bash
$ sudo apt install -y libgflags-dev libsnappy-dev zlib1g-dev libbz2-dev liblz4-dev libzstd-dev
$ git clone -b v6.11.4 --depth 1 https://github.com/facebook/rocksdb && cd rocksdb
$ make shared_lib -j $(nproc) && sudo make install-shared
$ cd .. && rm -r rocksdb
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
$ sudo apt install gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev:arm64 libstd-rust-dev:arm64
```

If you use Rustup:

```bash
$ sudo apt install gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev:arm64
$ rustup target add aarch64-unknown-linux-gnu
```

If you decided to use the system rocksdb (recommended if the target OS supports it), you need the version from the other architecture:

```bash
$ sudo apt install librocksdb-dev:arm64
```

#### Preparing for cross compilation on a different (Debian-based) OS distribution/version
*Note: Unless you run into the below mentioned libc (GLIBC) version issue, avoiding the approach described in this section is faster and requires less disk space on the build host. You may want to try the above cross compilation approach first and only use this one here as the last resort.*

If your build system runs on a different OS distribution and/or release than the target system electrs is going to run on, you may run into `GLIBC` version issues like:
```
$ ./electrs --help
./electrs: /lib/arm-linux-gnueabihf/libm.so.6: version `GLIBC_2.29' not found (required by ./electrs)
```

To cross-compile electrs for a different (Debian based) target distribution you can use a [debootstrap](https://wiki.debian.org/Debootstrap) based approach. For example your build system may be a 64-bit Debian `stable` (bullseye) system and you want to cross-compile for an armv7l (32-bit) Debian `oldstable` (buster) target, like an Odroid HC1/HC2.

Install and setup debootstrap:

```
sudo apt install debootstrap
```

Next, create working directory for a `buster` based system and set it up:
```
mkdir debootstrap-buster
sudo debootstrap buster debootstrap-buster http://deb.debian.org/debian/
```
(This takes a while to download.)

Next, mount proc, sys and dev to the target system:
```
sudo mount -t proc /proc debootstrap-buster/proc
sudo mount --rbind /sys debootstrap-buster/sys
sudo mount --rbind /dev debootstrap-buster/dev
```

If you have checked out the electrs git reposity somewhere already and don't want to have a duplicate copy inside the debootstrap working directory, just mount bind the exiting directory into the chroot:
```
sudo mkdir -p debootstrap-buster/mnt/electrs
sudo mount --rbind ./electrs debootstrap-buster/mnt/electrs
```

chroot into the `buster` system and install the required dependencies to build electrs with a statically linked rocksdb:
```
sudo chroot debootstrap-buster /bin/bash

apt install curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
apt install clang cmake build-essential

# install target specific cross compiler (armhf/gnueabihf)
apt install gcc-arm-linux-gnueabihf g++-arm-linux-gnueabihf libc6-dev-armhf-cross
rustup target add arm-unknown-linux-gnueabihf
```

Cross-compile `electrs` release with a statically linked rocksdb for armv7l (armhf) with libatomic inside `buster` chroot: *(bindgen needs an include path to the sources (header files) provided by the libc6-dev-armhf-cross package)*
```
cd /mnt/electrs/
BINDGEN_EXTRA_CLANG_ARGS="-target arm-linux-gnueabihf -I/usr/arm-linux-gnueabihf/include/"\
RUSTFLAGS="-C linker=arm-linux-gnueabihf-gcc -C linker-args=-latomic"\
cargo build --locked --release --target arm-unknown-linux-gnueabihf
```

The built electrs binary will be in `/mnt/electrs/target/arm-unknown-linux-gnueabihf/release` within the chroot or can be accessed from outside the chroot respectively.

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

Or if you have installed librocksdb from source

```
$ ROCKSDB_INCLUDE_DIR=/usr/local/include ROCKSDB_LIB_DIR=/usr/local/lib cargo build --locked --release
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
