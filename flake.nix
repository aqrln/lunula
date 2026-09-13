{
  description = "Lunula development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsForSystem =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
      rustToolchainForSystem =
        system: (pkgsForSystem system).rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsForSystem system;
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              qemu
              (rustToolchainForSystem system)
            ];
          };
        }
      );
    };
}
