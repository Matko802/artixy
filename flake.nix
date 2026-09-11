{
  description = "artixy - Discord bot managing an Artix VM via libvirt";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (nixpkgs.legacyPackages.${system}));

      artixy =
        { pkgs, mold }:
        pkgs.rustPlatform.buildRustPackage {
          pname = "artixy";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ mold ];
          RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
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
      packages = forAllSystems (pkgs:
        let
          staticBuild = artixy { pkgs = pkgs.pkgsStatic; mold = pkgs.mold; };
        in
        {
          default = pkgs.runCommand "artixy" { } ''
            mkdir -p $out/bin
            install -Dm755 ${staticBuild}/bin/artixy $out/bin/artixy
          '';
          artixy = pkgs.runCommand "artixy" { } ''
            mkdir -p $out/bin
            install -Dm755 ${staticBuild}/bin/artixy $out/bin/artixy
          '';
        });

      overlays.default = final: _prev: {
        artixy = artixy { pkgs = final.pkgsStatic; mold = final.mold; };
      };

      devShells = forAllSystems (pkgs:
        pkgs.mkShell {
          buildInputs = [ pkgs.pkgsMusl.cargo pkgs.pkgsMusl.rustc ];
        });
    };
}
