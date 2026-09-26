#!/usr/bin/env bash
# Build artixy (Go) for Raspberry Pi (linux/arm64) and install it as a
# systemd user service on pi@raspberrypi.local (plain Raspberry Pi OS).
#
# Usage:
#   ./pi-install.sh                 # build + copy + enable + restart
#   ./pi-install.sh --build-only    # only cross-compile ./artixy-linux-arm64
#
# Prerequisites:
#   - ssh access to pi@raspberrypi.local (password or key)
#   - Go toolchain on this machine (nix develop / system go)
#   - libvirt + qemu-guest-agent tools installed ON THE PI if you use VM commands
#     (sudo apt install -y libvirt-clients), plus ~/.config/artixy/config.toml
#     with discord_token / owner_id / vm_name.
set -euo pipefail
cd "$(dirname "$0")"

PI="${PI:-matko@raspberrypi.local}"
OUT="artixy-linux-arm64"

if ! command -v go >/dev/null 2>&1; then
  if [ -x /nix/store/4l04h8656as4i291mpcg38wz9m4k8acp-go-1.26.7/bin/go ]; then
    export PATH="/nix/store/4l04h8656as4i291mpcg38wz9m4k8acp-go-1.26.7/bin:$PATH"
  else
    echo "error: go not found in PATH" >&2
    exit 1
  fi
fi

echo "==> building linux/arm64..."
export CGO_ENABLED=0
GOOS=linux GOARCH=arm64 go build -trimpath -ldflags "-s -w" -o "$OUT" .

if [ "${1:-}" = "--build-only" ]; then
  echo "built ./$OUT"
  exit 0
fi

echo "==> copying to $PI:~/artixy/ ..."
ssh "$PI" "mkdir -p ~/artixy ~/.config/artixy ~/.config/systemd/user"
scp "$OUT" "$PI:~/artixy/artixy"
scp artixy.service "$PI:~/.config/systemd/user/artixy.service"
# Seed an empty share dir for /send (no-op if it exists).
ssh "$PI" "mkdir -p ~/artixy/share"

echo "==> enabling service on Pi ..."
ssh "$PI" "systemctl --user daemon-reload && systemctl --user enable --now artixy && loginctl enable-linger \"\$(whoami)\" || true"
ssh "$PI" "systemctl --user status artixy --no-pager | head -n 15 || true"

cat <<EOF
Done. Next on the Pi (ssh $PI):
  1. Edit ~/.config/artixy/config.toml (discord_token, owner_id, vm_name).
     Copy yours from this machine if you like:
       scp ~/.config/artixy/config.toml $PI:~/.config/artixy/config.toml
       ssh $PI "chmod 600 ~/.config/artixy/config.toml"
  2. systemctl --user restart artixy && journalctl --user -u artixy -f
EOF
