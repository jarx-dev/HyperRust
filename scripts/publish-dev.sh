#!/usr/bin/env bash
# Builds the Windows EXE on this Linux machine (cargo-xwin) and publishes it for
# the app's self-updater on a private server: <dir>/HyperRust-<build>.exe +
# <dir>/latest.json. The update server (scripts/hyperrust-updates.service)
# serves <dir> over Tailscale; running copies pick the build up within ~30 s.
# Public releases go through .github/workflows/release.yml instead.
#
#   scripts/publish-dev.sh            # fast build (profile dev-release)
#   scripts/publish-dev.sh --release  # same optimisation as CI (fat LTO)
#
# Env: HYPERRUST_UPDATE_DIR (default ~/hyperrust-updates), KEEP (default 5 builds).
# Settings that are this machine's own (addresses in the private network) live
# outside the repository, in ~/.config/hyperrust/build.env (shell variables;
# HYPERRUST_CONF points elsewhere) and are built into the EXE:
#   HYPERRUST_DEFAULT_UPDATE_URL   where new installations look for updates
#   HYPERRUST_DEFAULT_HISTORY_URL  history server new installations upload to
#   HYPERRUST_DEFAULT_LLM_URL      OpenAI-compatible endpoint new installations start with
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
out=${HYPERRUST_UPDATE_DIR:-$HOME/hyperrust-updates}
keep=${KEEP:-5}
profile=dev-release
[[ ${1:-} == --release ]] && profile=release
target=x86_64-pc-windows-msvc

# The cargo wrapper schedules builds through the user's systemd instance.
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
export DBUS_SESSION_BUS_ADDRESS=${DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus}

cd "$repo"
conf=${HYPERRUST_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/hyperrust/build.env}
if [[ -f $conf ]]; then set -a; . "$conf"; set +a; fi
# a private build without it would look for updates on GitHub, not on this server
: "${HYPERRUST_DEFAULT_UPDATE_URL:?brak HYPERRUST_DEFAULT_UPDATE_URL (plik $conf)}"
built_at=$(date +%s)
sha=$(git rev-parse --short HEAD)
git diff --quiet HEAD || sha="$sha-dirty"
export HYPERRUST_BUILT_AT=$built_at HYPERRUST_BUILD="$sha-$built_at"
notes=$(git log -1 --format=%s)

pnpm install --frozen-lockfile >/dev/null
pnpm build
# Plain cargo (not the Tauri CLI) so the custom profile works; custom-protocol
# embeds dist/ instead of pointing the webviews at the dev server.
(cd src-tauri && cargo xwin build --profile "$profile" --target "$target" --features tauri/custom-protocol)

exe="src-tauri/target/$target/$profile/hyperrust.exe"
[[ -f $exe ]] || { echo "brak $exe" >&2; exit 1; }

mkdir -p "$out"
name="HyperRust-$HYPERRUST_BUILD.exe"
cp "$exe" "$out/$name.part" && mv "$out/$name.part" "$out/$name"
sum=$(sha256sum "$out/$name" | cut -d' ' -f1)
size=$(stat -c %s "$out/$name")
version=$(sed -n 's/^version = "\(.*\)"/\1/p' src-tauri/Cargo.toml | head -1)
jq -n --arg version "$version" --arg build "$HYPERRUST_BUILD" --argjson built_at "$built_at" \
  --arg sha256 "$sum" --arg url "$name" --argjson size "$size" --arg notes "$notes" \
  '{version:$version, build:$build, built_at:$built_at, sha256:$sha256, url:$url, size:$size, notes:$notes}' \
  > "$out/latest.json.part"
mv "$out/latest.json.part" "$out/latest.json"

# Keep the newest $keep builds.
ls -1t "$out"/HyperRust-*.exe 2>/dev/null | tail -n +"$((keep + 1))" | while read -r old; do rm -f -- "$old"; done

echo "opublikowano $name ($((size / 1024)) KiB, $profile) → $out/latest.json"
