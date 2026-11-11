#!/usr/bin/env bash
# Daily post-processing model ranking on the private server, run by
# hyperrust-ranking.service from its own checkout (<data>/src, moved to the
# tested ref by the unit before this starts): builds examples/ranking.rs,
# publishes ranking.json next to the update manifest, where private builds read
# it, and a copy without the judge's remarks as the asset of the GitHub release
# `ranking` (a pre-release, so the updater's releases/latest never points at
# it), where public builds read it. `gh` must be logged in.
#
# API keys: OpenRouter's from Bitwarden Secrets Manager (`bws`, token in
# ~/.config/bws/token), CLIProxy's (the judge) from its config. They are never
# printed. This machine's own settings live outside the repository, in
# ~/.config/hyperrust/build.env (HYPERRUST_CONF points elsewhere):
#   HYPERRUST_RANKING_BWS_PROJECT  BWS project holding the OpenRouter key
#   CLIPROXY_BASE_URL              the judge's OpenAI-compatible endpoint
#   CLIPROXY_CONFIG                CLIProxy's config.yaml (default ~/cliproxyapi/config.yaml)
#   HYPERRUST_RANKING_REPO         GitHub repository of the public copy (default jarx-dev/HyperRust)
# Env: HYPERRUST_RANKING_DIR (default ~/hyperrust-ranking),
#      HYPERRUST_UPDATE_DIR (default ~/hyperrust-updates).
set -euo pipefail
conf=${HYPERRUST_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/hyperrust/build.env}
if [[ -f $conf ]]; then set -a; . "$conf"; set +a; fi
: "${HYPERRUST_RANKING_BWS_PROJECT:?brak HYPERRUST_RANKING_BWS_PROJECT (plik $conf)}"
: "${CLIPROXY_BASE_URL:?brak CLIPROXY_BASE_URL (plik $conf)}"
export CLIPROXY_BASE_URL
data=${HYPERRUST_RANKING_DIR:-$HOME/hyperrust-ranking}
publish=${HYPERRUST_UPDATE_DIR:-$HOME/hyperrust-updates}/ranking.json
src=$(cd "$(dirname "$0")/.." && pwd)

# A debug build is enough: the run waits on the network, not the CPU.
export CARGO_TARGET_DIR=$data/target
cargo build -q --manifest-path "$src/src-tauri/Cargo.toml" -p hyperrust-core --example ranking

BWS_ACCESS_TOKEN=$(tr -d '\n' < "$HOME/.config/bws/token")
export BWS_ACCESS_TOKEN
project=$(bws project list | jq -r --arg n "$HYPERRUST_RANKING_BWS_PROJECT" '.[] | select(.name == $n) | .id' | head -n1)
OPENROUTER_API_KEY=$(bws secret list "$project" | jq -r '.[] | select(.key == "OpenRouter") | .value' | head -n1)
CLIPROXY_API_KEY=$(grep -A3 '^api-keys:' "${CLIPROXY_CONFIG:-$HOME/cliproxyapi/config.yaml}" | sed -n 2p | sed -E "s/^ *- *['\"]?([^'\"]+)['\"]?.*/\1/")
unset BWS_ACCESS_TOKEN
if [ -z "$OPENROUTER_API_KEY" ] || [ -z "$CLIPROXY_API_KEY" ]; then
  echo "brak klucza OpenRouter (BWS) albo CLIProxy (config.yaml)" >&2
  exit 1
fi
export OPENROUTER_API_KEY CLIPROXY_API_KEY

"$CARGO_TARGET_DIR/debug/examples/ranking" --data "$data" --publish "$publish"

# Public copy for releases from GitHub: the models' results only. The judge's
# remarks quote the dictations, and part of the set is private.
public=$(mktemp -d)
trap 'rm -rf "$public"' EXIT
jq '.models[].issues = []' "$publish" > "$public/ranking.json"
gh release upload ranking "$public/ranking.json" --clobber -R "${HYPERRUST_RANKING_REPO:-jarx-dev/HyperRust}"
