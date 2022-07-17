{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable-small";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };
  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
      in
      with pkgs; {
        devShells.default = mkShell rec {
          TOOLCHAIN = rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          nativeBuildInputs = [
            TOOLCHAIN
            (python3.withPackages (ps: with ps;[ colorama ]))
            (callPackage ./cbmc.nix { })
          ];
        };
      }
    );
}

