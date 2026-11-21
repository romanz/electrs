{
  description = "Reproducible release build of electrs";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, fenix }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      lib = pkgs.lib;
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
      # Oldest glibc the binaries run on.
      glibc = "2.28";
      targets = {
        x86_64 = "x86_64-unknown-linux-gnu";
        aarch64 = "aarch64-unknown-linux-gnu";
      };

      rust = {
        channel = "1.99.1";
        sha256 = "sha256-+xUr6Al1wZSqX8aEsq7DpuFOK4Ve/i+qTl+K5uWQ9GM=";
      };
      toolchain = fenix.packages.${system}.combine (
        [
          (fenix.packages.${system}.toolchainOf rust).cargo
          (fenix.packages.${system}.toolchainOf rust).rustc
        ]
        ++ map (target: (fenix.packages.${system}.targets.${target}.toolchainOf rust).rust-std)
          (builtins.attrValues targets)
      );
      rustPlatform = pkgs.makeRustPlatform {
        cargo = toolchain;
        rustc = toolchain;
      };

      src = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [
          ./Cargo.toml
          ./Cargo.lock
          ./build.rs
          ./src
          ./internal
          ./LICENSE
        ];
      };

      # zig links against the given glibc version and statically against its own libc++,
      # so the binary only needs the system glibc.
      electrs = arch: rustPlatform.buildRustPackage {
        pname = "electrs";
        inherit version src;
        cargoLock.lockFile = ./Cargo.lock;

        nativeBuildInputs = [
          pkgs.cargo-zigbuild
          rustPlatform.bindgenHook
        ];

        buildPhase = ''
          export HOME=$TMPDIR
          export XDG_CACHE_HOME=$TMPDIR/xdg_cache
          export ZIG_GLOBAL_CACHE_DIR=$TMPDIR/zig_cache
          export CARGO_ZIGBUILD_CACHE_DIR=$TMPDIR/cargo_zigbuild_cache
          cargo zigbuild --release --locked --offline --target ${targets.${arch}}.${glibc}
        '';
        installPhase = ''
          install -Dm755 target/${targets.${arch}}/release/electrs $out/bin/electrs
        '';

        # Keep the standard interpreter and no rpath, so it runs outside nix.
        dontPatchELF = true;
        doCheck = false;
      };

      binaries = lib.mapAttrs (arch: _: electrs arch) targets;

      # The release tarballs and their SHA256SUMS, byte for byte, so anyone can check
      # the published assets.
      release = pkgs.runCommand "electrs-${version}-release" {
        nativeBuildInputs = [ pkgs.gnutar pkgs.gzip ];
      } ''
        mkdir $out
        ${lib.concatStrings (lib.mapAttrsToList (arch: bin: ''
          dir=electrs-${version}
          mkdir $dir
          cp ${bin}/bin/electrs $dir/electrs
          cp ${./LICENSE} $dir/LICENSE
          chmod 0755 $dir $dir/electrs
          chmod 0644 $dir/LICENSE
          tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -cf - $dir \
            | gzip -9n > $out/electrs-${version}-${arch}-linux-gnu.tar.gz
          rm -r $dir
        '') binaries)}
        (cd $out && sha256sum *.tar.gz > SHA256SUMS)
      '';
    in {
      packages.${system} = binaries // {
        inherit release;
        default = release;
      };
    };
}
