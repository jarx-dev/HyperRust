#!/usr/bin/env bash
# README pictures (docs/screenshots) from demo data. Builds the frontend with
# scripts/screenshots/mock.js standing in for the Tauri API and renders each page in
# headless Edge on a Windows machine over ssh: Segoe UI and the same engine as WebView2,
# and headless means nothing ever shows on that machine's screen.
#
#   scripts/screenshots.sh           # Windows over ssh, pictures into docs/screenshots
#   UI=en scripts/screenshots.sh     # the English UI, pictures into docs/screenshots/en
#   LOCAL=1 scripts/screenshots.sh   # headless Chrome here, a quick preview (other fonts)
#
# The ssh host is this machine's own setting: WIN in ~/.config/hyperrust/build.env
# (HYPERRUST_CONF points elsewhere), never in the repository. The Windows side works in
# %TEMP%\hyperrust-shots and touches nothing else.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
ui=${UI:-pl}
[[ $ui == pl || $ui == en ]] || { echo "UI musi być pl albo en" >&2; exit 1; }
out=$repo/docs/screenshots
[[ $ui == pl ]] || out=$out/$ui
cd "$repo"
conf=${HYPERRUST_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/hyperrust/build.env}
if [[ -f $conf ]]; then set -a; . "$conf"; set +a; fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
site=$tmp/hyperrust-shots
pnpm exec vite build --base ./ --outDir "$site" --emptyOutDir --logLevel warn
cp scripts/screenshots/mock.js "$site/"
# file:// pages: the mock must run before the app's module, and `crossorigin` would block it
for page in index.html overlay.html; do
  sed -i -e 's/ crossorigin//' -e "0,/<script type=\"module\"/s//<script>window.MOCK_LANG = \"$ui\"<\/script>\n    <script src=\".\/mock.js\"><\/script>\n    &/" "$site/$page"
done
# headless windows have a minimum width: pin the indicator to its real window size
sed -i 's|</head>|<style>html,body{width:338px!important;height:72px!important}</style></head>|' "$site/overlay.html"

# name  page            window   theme
shots=(
  "overview index.html#overview 1000,760 light"
  "modes    index.html#modes    1000,760 light"
  "history  index.html#history  1000,760 light"
  "about    index.html#general  1000,760 light"
  "dark     index.html#overview 1000,760 dark"
  "overlay  overlay.html        338,72   dark"
)
flags=(--headless=new --disable-gpu --no-first-run --hide-scrollbars --allow-file-access-from-files
  --force-device-scale-factor=2 --virtual-time-budget=4000 --default-background-color=00000000)
# Render in a window at least 600 px wide and BAR px taller (headless Chrome counts its
# browser bar in the window height, headless Edge does not), then cut to the real size.
crop() { python3 "$repo/scripts/screenshots/crop.py" "$1" "$((${2%,*} * 2))" "$((${2#*,} * 2))"; }
window() { local w=${1%,*}; echo "$((w > 600 ? w : 600)),$((${1#*,} + BAR))"; }
theme_flags() { [[ $1 == dark ]] && echo "--force-dark-mode --blink-settings=preferredColorScheme=0" || echo "--blink-settings=preferredColorScheme=1"; }

mkdir -p "$out"
if [[ -n ${LOCAL:-} ]]; then
  BAR=88
  chrome=$(command -v google-chrome || command -v chromium)
  for s in "${shots[@]}"; do
    read -r name page size theme <<<"$s"
    # shellcheck disable=SC2046
    "$chrome" "${flags[@]}" $(theme_flags "$theme") --user-data-dir="$tmp/profile" \
      --window-size="$(window "$size")" --screenshot="$out/$name.png" "file://$site/$page" 2>/dev/null
    crop "$out/$name.png" "$size"
  done
else
  : "${WIN:?brak WIN (plik $conf)}"
  BAR=0
  wtemp=$(ssh -o BatchMode=yes "$WIN" 'cmd /c echo %TEMP%' | tr -d '\r"' | tr '\\' /)
  scp -q -r "$site" "$WIN:$wtemp/"
  edge='C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe'
  for s in "${shots[@]}"; do
    read -r name page size theme <<<"$s"
    ssh -o BatchMode=yes "$WIN" "cmd /c \"\"$edge\" ${flags[*]} $(theme_flags "$theme") --user-data-dir=\"$wtemp/hyperrust-shots/profile\" --window-size=$(window "$size") --screenshot=\"$wtemp/hyperrust-shots/$name.png\" \"file:///$wtemp/hyperrust-shots/$page\"\"" >/dev/null 2>&1
    scp -q "$WIN:$wtemp/hyperrust-shots/$name.png" "$out/$name.png"
    crop "$out/$name.png" "$size"
  done
fi
ls -l "$out"
