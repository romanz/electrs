## Native OS packages

There are currently no official/stable binary packages.

However, there's a [*beta* repository for Debian 10](https://deb.ln-ask.me) (should work on recent Ubuntu, but not tested well-enough)
The repository provides several significant advantages:

* Everything is completely automatic - after installing `electrs` via `apt`, it's running and will automatically run on reboot, restart after crash..
  It also connects to bitcoind out-of-the-box, no messing with config files or anything else.
  It just works.
* Prebuilt binaries save you a lot of time.
  The binary installation of all the components is under 3 minutes on common hardware.
  Building from source is much longer.
* The repository contains some security hardening out-of-the-box - separate users for services, use of [btc-rpc-proxy](https://github.com/Kixunil/btc-rpc-proxy), etc.

And two disadvantages:

* It's currently not trivial to independently verify the built packages, so you may need to trust the author of the repository.
  The build is now deterministic but nobody verified it independently yet.
* The repository is considered beta.
  electrs seems to work well so far but was not tested heavily.
  The author of the repository is also a contributor to `electrs` and appreciates [bug reports](https://github.com/Kixunil/cryptoanarchy-deb-repo-builder/issues),
  [test reports](https://github.com/Kixunil/cryptoanarchy-deb-repo-builder/issues/61), and other contributions.

## Reproducible release binaries

Release binaries for x86_64 and aarch64 Linux can be built with [Nix](https://nixos.org/download/)
and [just](https://github.com/casey/just), from any `x86_64-linux` machine:

```bash
$ git clone https://github.com/romanz/electrs && cd electrs
$ git checkout v<version>  # the release to build
$ just release
$ ls dist/
electrs-<version>-aarch64-linux-gnu.tar.gz  electrs-<version>-x86_64-linux-gnu.tar.gz  SHA256SUMS
```

The build is pinned by `flake.lock` (including the Rust toolchain and [zig](https://ziglang.org/)),
so every machine produces the same tarballs, byte for byte.
The binaries run on any Linux with glibc 2.28 or newer (Debian 10, Ubuntu 20.04, RHEL 8 and later).

To publish a release, the maintainer builds it on their own machine and signs the checksums:

```bash
$ just sign
```

then attaches the content of `dist/` (the tarballs, `SHA256SUMS` and `SHA256SUMS.asc`) to the release.

To verify a release, check the signature, then rebuild the same tag and compare:

```bash
$ gpg --verify SHA256SUMS.asc SHA256SUMS
$ git verify-tag v<version>
$ just release
$ (cd dist && sha256sum -c ../SHA256SUMS)
```
