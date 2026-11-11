#!/usr/bin/env bash
# Installs the daily model ranking (hyperrust-ranking.timer) as systemd user
# units on this machine, with its own checkout in ~/hyperrust-ranking/src.
#
# The dictation set is not in the repo: ~/hyperrust-ranking/set/hist.jsonl
# holds history lines (`transcript`) of the real dictations it tests with.
# HYPERRUST_RANKING_REF=origin/<branch> pins the checkout to a branch (until
# the ranking is merged into main); without it the units follow origin/main.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
data=${HYPERRUST_RANKING_DIR:-$HOME/hyperrust-ranking}
units=$HOME/.config/systemd/user
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}

if [ ! -s "$data/set/hist.jsonl" ]; then
  echo "brak $data/set/hist.jsonl: najpierw skopiuj tam dyktowania zestawu" >&2
  exit 1
fi
if [ ! -d "$data/src/.git" ]; then
  git clone -q "$(git -C "$here" remote get-url origin)" "$data/src"
fi
mkdir -p "$units"
install -m 0644 "$here/hyperrust-ranking.service" "$units/hyperrust-ranking.service"
install -m 0644 "$here/hyperrust-ranking.timer" "$units/hyperrust-ranking.timer"
dropin=$units/hyperrust-ranking.service.d
if [ -n "${HYPERRUST_RANKING_REF:-}" ]; then
  mkdir -p "$dropin"
  printf '[Service]\nEnvironment=HYPERRUST_RANKING_REF=%s\n' "$HYPERRUST_RANKING_REF" > "$dropin/ref.conf"
else
  rm -f "$dropin/ref.conf"
fi
systemctl --user daemon-reload
systemctl --user enable --now hyperrust-ranking.timer
systemctl --user list-timers --no-pager hyperrust-ranking.timer
