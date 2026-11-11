#!/usr/bin/env bash
# Installs and starts the dictation history server as a systemd user unit on this machine.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
mkdir -p "$HOME/.local/lib/hyperrust" "$HOME/.config/systemd/user"
mkdir -p -m 0700 "$HOME/hyperrust-history"
install -m 0755 "$here/history-server.py" "$HOME/.local/lib/hyperrust/history-server.py"
install -m 0644 "$here/hyperrust-history.service" "$HOME/.config/systemd/user/hyperrust-history.service"
systemctl --user daemon-reload
systemctl --user enable --now hyperrust-history.service
systemctl --user restart hyperrust-history.service
systemctl --user --no-pager status hyperrust-history.service | head -5
