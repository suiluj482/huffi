{
  description = "huffi - a launcher that learns what you meant, not just what you use";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils, systems, ... }:
    let
      huffiModule = import ./nix/home-manager.nix;
    in
    {
      overlays.default = final: prev: {
        huffi = final.callPackage ./nix/default.nix { };
      };
      
      homeManagerModules.default = huffiModule;
      homeManagerModules.huffi = huffiModule;
    } //
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        huffi = pkgs.callPackage ./nix/default.nix { };
      in
      {
        packages = {
          inherit huffi;
          default = huffi;
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
            pkg-config
            cargo-deny
          ];

          buildInputs = with pkgs; [
            wayland
            wayland-protocols
            libxkbcommon
            vulkan-loader
            libGL
            mesa
            libxcursor
            libxrandr
            libxi
            gtk4
            gtk4-layer-shell
            librsvg
          ];

          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
            pkgs.wayland
            pkgs.libxkbcommon
            pkgs.vulkan-loader
            pkgs.libGL
            pkgs.mesa
            pkgs.libxcursor
            pkgs.libxrandr
            pkgs.libxi
          ];
        };
      }
    );
}
