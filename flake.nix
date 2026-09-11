{
  description = "artixy - Discord bot managing an Artix VM via libvirt";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (nixpkgs.legacyPackages.${system}));

      artixy =
        { pkgs }:
        pkgs.rustPlatform.buildRustPackage {
          pname = "artixy";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          meta = {
            mainProgram = "artixy";
            description = "Discord bot managing an Artix VM via libvirt";
            homepage = "https://github.com/Matko802/artixy";
            license = pkgs.lib.licenses.mit;
            platforms = pkgs.lib.platforms.linux;
          };
        };
    in
    {
      packages = forAllSystems (pkgs: {
        default = artixy { pkgs = pkgs; };
        artixy = artixy { pkgs = pkgs; };
      });

      overlays.default = final: _prev: {
        artixy = artixy { pkgs = final; };
      };

      devShells = forAllSystems (pkgs:
        pkgs.mkShell {
          buildInputs = with pkgs; [ cargo rustc ];
        });
    };
}
