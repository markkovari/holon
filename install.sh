#!/usr/bin/env sh
# Install Holon's native binaries on macOS or Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/markkovari/holon/main/install.sh | sh
#   curl -fsSL .../install.sh | HOLON_ROLE=full sh
#
# There are no release artifacts yet (no tags, no `gh release`), so this builds
# from a source tarball rather than downloading one. That is the honest version:
# a script that pretends to fetch a binary and 404s is worse than one that takes
# four minutes and works. When a release workflow exists, the fetch goes in front
# of the build and the rest of this file does not change.
#
#   HOLON_ROLE    worker (default) | full
#   HOLON_BINS    explicit space-separated list; overrides HOLON_ROLE
#   HOLON_REF     git ref to install, default main
#   HOLON_HOME    install prefix, default ~/.holon  (binaries land in $HOLON_HOME/bin)
#   HOLON_SRC     an existing checkout to build from; skips the download
set -eu

REF="${HOLON_REF:-main}"
HOME_DIR="${HOLON_HOME:-$HOME/.holon}"
BIN_DIR="$HOME_DIR/bin"
REPO="${HOLON_REPO:-markkovari/holon}"

# `worker` is the set a second machine needs to take gate work: comp-checks runs
# a candidate's checks in a throwaway tree, and it materialises the tree from the
# request — so a worker needs no checkout of the project it is gating.
#
# `full` is a box that also drives the loop and serves apps.
case "${HOLON_ROLE:-worker}" in
  worker) DEFAULT_BINS="comp-checks" ;;
  full)   DEFAULT_BINS="comp-checks comp-host comp-plug comp-field comp-relay comp-goalrun comp-goald holon" ;;
  *)      echo "install.sh: HOLON_ROLE must be 'worker' or 'full', got '${HOLON_ROLE}'" >&2; exit 2 ;;
esac
BINS="${HOLON_BINS:-$DEFAULT_BINS}"

# Which workspace each binary lives in. Holon is five cargo workspaces, not one,
# so `cargo build --bin comp-host` from the root finds nothing.
workspace_of() {
  case "$1" in
    comp-host) echo host ;;
    holon)     echo cli ;;
    *)         echo reconciler ;;
  esac
}

os="$(uname -s)"; arch="$(uname -m)"
case "$os" in
  Darwin|Linux) ;;
  *) echo "install.sh: unsupported OS '$os' — macOS and Linux only" >&2; exit 2 ;;
esac

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "install.sh: '$1' is required and not on PATH." >&2
    [ "$1" = cargo ] && echo "  install Rust: https://rustup.rs" >&2
    exit 2
  }
}
need cargo
need tar

if [ -n "${HOLON_SRC:-}" ]; then
  src="$HOLON_SRC"
  [ -f "$src/reconciler/Cargo.toml" ] || {
    echo "install.sh: HOLON_SRC='$src' is not a Holon checkout (no reconciler/Cargo.toml)" >&2
    exit 2
  }
  echo ">> building from $src"
  cleanup=""
else
  need curl
  src="$(mktemp -d)"
  cleanup="$src"
  url="https://github.com/$REPO/archive/$REF.tar.gz"
  echo ">> $url"
  # --strip-components=1: the archive's top directory is named after the ref, and
  # a ref with a slash in it ('feat/x') makes that name unpredictable.
  curl -fsSL "$url" | tar -xzf - --strip-components=1 -C "$src" || {
    echo "install.sh: could not fetch or unpack $url" >&2
    exit 1
  }
fi
trap '[ -n "$cleanup" ] && rm -rf "$cleanup"' EXIT INT TERM

echo ">> building for $os/$arch: $BINS"
mkdir -p "$BIN_DIR"
for bin in $BINS; do
  ws="$(workspace_of "$bin")"
  ( cd "$src/$ws" && cargo build --release --quiet --bin "$bin" ) || {
    echo "install.sh: building $bin (in $ws/) failed" >&2
    exit 1
  }
  cp "$src/$ws/target/release/$bin" "$BIN_DIR/$bin"
done

# The one runnable check: an installed binary that cannot answer --help is a
# binary that was copied from the wrong place or built for the wrong libc.
for bin in $BINS; do
  "$BIN_DIR/$bin" --help >/dev/null 2>&1 || {
    echo "install.sh: $BIN_DIR/$bin was installed but does not run" >&2
    exit 1
  }
done

echo ">> installed to $BIN_DIR:"
for bin in $BINS; do echo "     $bin"; done

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo
    echo "   Add it to PATH:"
    echo "     bash/zsh   echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.profile"
    echo "     fish       fish_add_path $BIN_DIR"
    ;;
esac
