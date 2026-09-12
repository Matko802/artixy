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
          # Precise file set: the old cleanSource copied the whole 5GB+
          # target/ dir into the store (hash + copy) on every build.
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./imgs
            ];
          };
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
          # GNU toolchain for fast iteration (dynamic linking); the static
          # musl deploy build is unaffected (separate derivation above).
          buildInputs = [ pkgs.cargo pkgs.rustc pkgs.sccache ];
          shellHook = ''
            export RUSTC_WRAPPER=sccache
            export SCCACHE_DIR="''${SCCACHE_DIR:-$HOME/.cache/sccache}"
          '';
        });
    };
}
