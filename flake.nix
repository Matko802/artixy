# artixy — Discord bot managing an Artix VM via libvirt.
#
# Build a dev binary with `cargo build`, deploy (Artix host, non-Nix) with
# `cargo build --release && systemctl --user restart artixy`.
#
# For a NixOS host use the bundled flake + module (see flake.nix /
# modules/artixy.nix). The `build` / `botrestart` Discord commands are
# runtime-gated by deployed_via_nix() in src/main.rs: under a read-only
# /nix/store binary they refuse and point you at `nixos-rebuild` /
# `systemctl --user restart artixy` instead of trying to mutate the store.
{
  description = "artixy — Discord bot managing an Artix VM via libvirt";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;

      artixyPkg = system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "artixy";
          version = "0.1.0";
          src = self;
          # Nix pins every dependency from Cargo.lock; update with
          # `cargo update` then bump cargoLock.lockFile.
          cargoLock.lockFile = ./Cargo.lock;
          # Unit tests talk to a live VM + Discord; they can't run in the
          # isolated Nix sandbox, and `build`/`botrestart` mutate the store
          # which is read-only here. So: no tests, no self-rebuild.
          doCheck = false;
          meta.mainProgram = "artixy";
        };
    in
    {
      packages = forAllSystems (system: {
        default = artixyPkg system;
        artixy = artixyPkg system;
      });

      # Wire up in configuration.nix:
      #   {
      #     imports = [ inputs.artixy.nixosModules.artixy ];
      #     services.artixy = {
      #       enable     = true;
      #       envFile    = "/home/matko/artixy/.env";   # DISCORD_TOKEN / OWNER_ID / VM_NAME
      #       workingDir = "/home/matko/artixy";        # users.json / settings.json / shells.json
      #     };
      #   }
      # Deploy:  nixos-rebuild switch --flake .
      # Restart: systemctl --user restart artixy
      nixosModules.artixy  = import ./modules/artixy.nix self;
      nixosModules.default = self.nixosModules.artixy;
    };
}
