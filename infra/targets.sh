#!/usr/bin/env bash
# Every cargo workspace's target/ lives in one folder outside the checkout, each in-repo target/ a
# link there, so the paths scripts and tests name (target/release/...) still work.
#
#   ./targets.sh link    link each workspace's target/ there, moving what it already built
#   ./targets.sh clean   empty that folder, and remove any target/ in the checkout that is not a link
#   ./targets.sh du      how much each takes
#
# HOLON_TARGETS picks the folder (default ~/.cache/holon-target). `cargo clean` removes the link
# itself; the next `link` puts it back.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
TARGETS="${HOLON_TARGETS:-$HOME/.cache/holon-target}"
cd "$REPO"

workspaces() {
  echo .
  git ls-files '*Cargo.toml' | xargs grep -l '^\[workspace\]' | xargs -n1 dirname | grep -vx .
}

# root for the checkout's own, else the path with / as -
name() { [ "$1" = . ] && echo root || echo "${1//\//-}"; }

case "${1:-}" in
  link)
    mkdir -p "$TARGETS"
    workspaces | while read -r ws; do
      dest="$TARGETS/$(name "$ws")" t="$ws/target"
      [ -L "$t" ] && [ "$(readlink "$t")" = "$dest" ] && { mkdir -p "$dest"; continue; }
      if [ -d "$t" ] && [ ! -L "$t" ]; then
        if [ -e "$dest" ]; then rm -rf "$t"; else mv "$t" "$dest"; fi
      fi
      mkdir -p "$dest"
      ln -sfn "$dest" "$t"
      echo "$t -> $dest"
    done ;;
  clean)
    find "$TARGETS" -mindepth 2 -maxdepth 2 -exec rm -rf {} + 2>/dev/null || true
    find . -name node_modules -prune -o -name target -type d -prune -print | while read -r t; do
      echo "removing $t (not a link)"
      rm -rf "$t"
    done
    echo "emptied $TARGETS" ;;
  du)
    du -sh "$TARGETS"/* 2>/dev/null | sort -rh
    find . -name node_modules -prune -o -name target -type d -prune -print | xargs -r du -sh ;;
  *) sed -n 2,10p "$0"; exit 1 ;;
esac
