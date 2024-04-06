{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
        flake-utils.follows = "flake-utils";
      };
    };
    crane = {
      url = "github:ipetkov/crane";
      inputs = {
        nixpkgs.follows = "nixpkgs";
      };
    };
  };
  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane }:
    flake-utils.lib.eachDefaultSystem
      (system:
        let
          overlays = [ (import rust-overlay) ];
          pkgs = import nixpkgs {
            inherit system overlays;
          };
          rustToolchain = pkgs.pkgsBuildHost.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

          src = craneLib.cleanCargoSource ./.; 

          nativeBuildInputs = with pkgs; [ rustToolchain clang ]; # required only at build time
          buildInputs = with pkgs; [ ]; # also required at runtime

          commonArgs = {
            inherit src buildInputs nativeBuildInputs;
            LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
            ELEMENTSD_SKIP_DOWNLOAD = true;
            BITCOIND_SKIP_DOWNLOAD = true;
            ELECTRUMD_SKIP_DOWNLOAD = true;
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          bin = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;

            # TODO: do testing by providing executables via *_EXE env var for {bitcoin,elements,electrum}d
            doCheck = false;
          });
          binLiquid = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
            cargoExtraArgs = "--features liquid";

            # TODO: do testing by providing executables via *_EXE env var for {bitcoin,elements,electrum}d
            doCheck = false;
          });


        in
        with pkgs;
        {
          packages =
            {
              # that way we can build `bin` specifically,
              # but it's also the default.
              inherit bin;
              default = bin;
            };

          apps."blockstream-electrs-liquid" = {
            type = "app";
            program = "${binLiquid}/bin/electrs";
          };
          apps."blockstream-electrs" = {
            type = "app";
            program = "${bin}/bin/electrs";
          };


          devShells.default = mkShell {
            inputsFrom = [ bin ];
            LIBCLANG_PATH = "${pkgs.libclang.lib}/lib"; # for rocksdb
          };
        }
      );
}
