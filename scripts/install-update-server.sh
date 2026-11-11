#!/usr/bin/env bash
# Installs and starts the update server as a systemd user unit on this machine.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
mkdir -p "$HOME/hyperrust-updates" "$HOME/.config/systemd/user"
install -m 0644 "$here/hyperrust-updates.service" "$HOME/.config/systemd/user/hyperrust-updates.service"
systemctl --user daemon-reload
systemctl --user enable --now hyperrust-updates.service
systemctl --user --no-pager status hyperrust-updates.service | head -5
