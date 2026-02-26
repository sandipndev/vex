{
  description = "vex - parallel workstream manager";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
      };
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    rust-overlay,
    crane,
  }:
    {
      overlays.default = final: prev: {
        vex = self.packages.${final.system}.vex;
      };
    }
    // flake-utils.lib.eachDefaultSystem
    (system: let
      overlays = [(import rust-overlay)];
      pkgs = import nixpkgs {
        inherit system overlays;
      };

      rustVersion = pkgs.pkgsBuildHost.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      rustToolchain = rustVersion.override {
        extensions = [
          "rust-analyzer"
          "rust-src"
          "rustfmt"
          "clippy"
        ];
      };
      craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

      rustSource = pkgs.lib.cleanSourceWith {
        src = ./.;
        filter = path: type:
          craneLib.filterCargoSources path type;
      };

      commonArgs = {
        src = rustSource;
        strictDeps = true;
        nativeBuildInputs =
          pkgs.lib.optionals pkgs.stdenv.isLinux [pkgs.clang pkgs.lld]
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [pkgs.llvmPackages.lld];
      };

      cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      vex = craneLib.buildPackage (commonArgs
        // {
          inherit cargoArtifacts;
          doCheck = false;
        });

      nativeBuildInputs = with pkgs; [
        rustToolchain
        alejandra
        cargo-watch
        bacon
      ];
    in
      with pkgs; {
        packages = {
          default = vex;
          vex = vex;
        };

        checks = {
          clippy = craneLib.cargoClippy (commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            });
          fmt = craneLib.cargoFmt {
            src = rustSource;
          };
        };

        devShells.default = mkShell {
          inherit nativeBuildInputs;
        };

        formatter = alejandra;
      });
}
