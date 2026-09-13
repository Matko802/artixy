{
  description = "artixy - Discord bot managing an Artix VM via libvirt";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    { self, nixpkgs, crane }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (nixpkgs.legacyPackages.${system}));

      mkArtixy = pkgs: mold:
        let
          craneLib = crane.mkLib pkgs;
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./imgs
            ];
          };
          # cache deps separately — src changes rebuild only artixy, not 243 crates
          cargoArtifacts = craneLib.buildDepsOnly {
            inherit src;
            strictDeps = true;
          };
        in craneLib.buildPackage {
          inherit src cargoArtifacts;
          strictDeps = true;
          nativeBuildInputs = [ mold ];
          RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
          doCheck = false;
        };

      artixy = { pkgs, mold }: mkArtixy pkgs mold;
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
