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
          overlays = [
            (import rust-overlay)
            (import ./rocksdb-overlay.nix)
          ];
          pkgs = import nixpkgs {
            inherit system overlays;
          };
          rustToolchain = pkgs.pkgsBuildHost.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

          src = craneLib.cleanCargoSource ./.;

          nativeBuildInputs = with pkgs; [ rustToolchain clang ]; # required only at build time
          buildInputs = with pkgs; [ ]; # also required at runtime

          envVars =
            {
              LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
              ELEMENTSD_SKIP_DOWNLOAD = true;
              BITCOIND_SKIP_DOWNLOAD = true;
              ELECTRUMD_SKIP_DOWNLOAD = true;

              # link rocksdb dynamically
              ROCKSDB_INCLUDE_DIR = "${pkgs.rocksdb}/include";
              ROCKSDB_LIB_DIR = "${pkgs.rocksdb}/lib";

              # for integration testing
              BITCOIND_EXE = "${pkgs.bitcoind}/bin/bitcoind";
              ELEMENTSD_EXE = "${pkgs.elementsd}/bin/elementsd";
              ELECTRUMD_EXE = "${pkgs.electrum}/bin/electrum";
            };

          commonArgs = {
            inherit src buildInputs nativeBuildInputs;
          } // envVars;

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          bin = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
          });
          binLiquid = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
            cargoExtraArgs = "--features liquid";
          });

        in
        with pkgs;
        {
          packages =
            {
              # that way we can build `bin` specifically,
              # but it's also the default.
              inherit bin binLiquid;
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

          devShells.default = mkShell (envVars // {
            inputsFrom = [ bin ];
          });
        }
      );
}
