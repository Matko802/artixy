# NixOS module for artixy — imported by flake.nix as
#   `import ./modules/artixy.nix self`  (self = the flake).
#
# Wire up in configuration.nix:
#   {
#     imports = [ inputs.artixy.nixosModules.artixy ];
#     services.artixy = {
#       enable     = true;
#       envFile    = "/home/matko/artixy/.env";   # DISCORD_TOKEN / OWNER_ID / VM_NAME
#       workingDir = "/home/matko/artixy";         # users.json / settings.json / shells.json
#     };
#   }
# Deploy:  nixos-rebuild switch --flake .
# Restart: systemctl --user restart artixy      # logs: journalctl --user -u artixy -f
#
# NOTES
# * Runs as a systemd *user* service (systemd.user.services), so the bot talks
#   to libvirt over your session URI (qemu:///session) — no host-level libvirt
#   group / system URI privileges required.
# * Nix keeps users.json / settings.json / shells.json OUTSIDE the store (in
#   workingDir) so bot data stays mutable while /nix/store stays immutable.
# * The `build` and `botrestart` Discord commands are runtime-gated by
#   deployed_via_nix() in src/main.rs: under a read-only /nix/store binary
#   they refuse and tell you to use `nixos-rebuild` / `systemctl --user
#   restart artixy` instead of trying to mutate the store.
self:
{ config, lib, pkgs, ... }:

let
  cfg = config.services.artixy;
  pkg = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
in
{
  options.services.artixy = {
    enable = lib.mkEnableOption "the artixy Discord bot";

    envFile = lib.mkOption {
      type = lib.types.path;
      description = ".env exporting DISCORD_TOKEN, OWNER_ID and VM_NAME.";
    };

    workingDir = lib.mkOption {
      type = lib.types.path;
      description = "Directory holding users.json / settings.json / shells.json.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.user.services.artixy = {
      description = "artixy Discord bot";
      wantedBy = [ "default.target" ];

      serviceConfig = {
        Type = "simple";
        ExecStart = "${pkg}/bin/artixy";
        WorkingDirectory = cfg.workingDir;
        EnvironmentFile = cfg.envFile;
        Restart = "on-failure";
        RestartSec = 5;
      };
    };
  };
}
