{
  description = "artixy - Discord bot managing an Artix VM via libvirt (Go)";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.buildGoModule {
          pname = "artixy";
          version = "0.2.0";
          src = ./.;
          # vendor/ is committed, so no hash needed and the build is sandbox-safe.
          vendorHash = null;
          ldflags = [
            "-s"
            "-w"
          ];
          meta = {
            mainProgram = "artixy";
            description = "Discord bot managing an Artix VM via libvirt";
            homepage = "https://github.com/Matko802/artixy";
            license = pkgs.lib.licenses.mit;
            platforms = pkgs.lib.platforms.linux;
          };
        };
      });

      overlays.default = final: _prev: {
        artixy = self.packages.${final.system}.default;
      };

      devShells = forAllSystems (pkgs:
        pkgs.mkShell {
          buildInputs = [
            pkgs.go
            pkgs.gopls
          ];
        });
    };
}
