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
