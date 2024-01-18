{
  inputs.flake-utils.url = github:numtide/flake-utils;
  description = "nix-source";

  outputs = { self, nixpkgs, fenix, flake-utils }:
  with flake-utils.lib;
    (eachSystem [ system.x86_64-linux ] (system: let
      pkgs = import nixpkgs { inherit system; };
      nix-source-with-pkgs = pkgs: pkgs.rustPlatform.buildRustPackage {
        pname = "nix-source";
        version = "0.0.1";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
      };
    in {
      packages = rec {
        default = nix-source;
        nix-source = nix-source-with-pkgs pkgs;
      };
    })) // {
      overlays.default = final: prev: {
        nix-source = nix-source-with-pkgs final;
      };
    };
}
