{
  inputs.fenix = {
    inputs.nixpkgs.follows = "nixpkgs";
    url = github:nix-community/fenix;
  };
  
  inputs.flake-utils = {
    url = "github:numtide/flake-utils";
    inputs.nixpkgs.follows = "nixpkgs";
  };
  description = "nix-source";

  outputs = { self, nixpkgs, fenix, flake-utils }:
  with flake-utils.lib;
    eachSystem [ system.x86_64-linux ] (system: let
      pkgs = import nixpkgs { inherit system; overlays = [ fenix.overlays.default ]; };
      rust-toolchain = pkgs.fenix.fromToolchainFile {
        file = ./rust-toolchain.toml;
        sha256 = "sha256-ukNdeGQZ9GYkiab9vgzZzBc5UI3+kHdNsgwVRcAb5TA=";
      };
      rustPlatform = pkgs.makeRustPlatform {
        cargo = rust-toolchain;
        rustc = rust-toolchain;
      };
      nix-source = rustPlatform.buildRustPackage {
        pname = "nix-source";
        version = "0.0.1";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
      };
    in {
      packages = {
        default = nix-source;
        nix-source = nix-source;
      };
    });
}
