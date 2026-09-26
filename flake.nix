{
  description = "artixy - Discord bot managing an Artix VM via libvirt (C)";

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
        default = pkgs.stdenv.mkDerivation {
          pname = "artixy";
          version = "0.3.0";
          src = ./.;
          nativeBuildInputs = [ pkgs.gnumake pkgs.pkg-config ];
          buildInputs = [
            pkgs.jansson
            pkgs.curlFull
            pkgs.libvterm-neovim
          ];
          buildPhase = ''
            make
          '';
          checkPhase = ''
            make test
          '';
          doCheck = true;
          installPhase = ''
            mkdir -p $out/bin
            install -Dm755 artixy $out/bin/artixy
          '';
          meta = {
            mainProgram = "artixy";
            description = "Discord bot managing an Artix VM via libvirt";
            homepage = "https://github.com/Matko802/artixy";
            license = pkgs.lib.licenses.mit;
            platforms = pkgs.lib.platforms.linux;
          };
        };
      });

      devShells = forAllSystems (pkgs:
        pkgs.mkShell {
          buildInputs = [
            pkgs.gcc
            pkgs.gnumake
            pkgs.pkg-config
            pkgs.jansson
            pkgs.curlFull
            pkgs.libvterm-neovim
            pkgs.valgrind
          ];
        });
    };
}
