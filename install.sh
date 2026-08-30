#!/usr/bin/env sh
# Install Holon's native binaries on macOS or Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/markkovari/holon/main/install.sh | sh
#   curl -fsSL .../install.sh | HOLON_ROLE=full sh
#
# Downloads a prebuilt binary. A machine taking gate work needs NO Rust: it needs
# `comp-checks`, which materialises the candidate tree from the request and runs
# the commands the checks name. Whatever toolchain THOSE need is the gated
# project's business — a worker gating a Python repo has no reason to hold a Rust
# compiler, and it will not get one from here.
#
# Falls back to building from source only when there is no release for this
# platform, and says so rather than doing it quietly.
#
#   HOLON_ROLE    worker (default) | full
#   HOLON_BINS    explicit space-separated list; forces the source build
#   HOLON_VERSION release tag to install, default the latest non-prerelease
#   HOLON_HOME    install prefix, default ~/.holon  (binaries land in $HOLON_HOME/bin)
#   HOLON_SRC     an existing checkout to build from; skips the download entirely
set -eu

HOME_DIR="${HOLON_HOME:-$HOME/.holon}"
BIN_DIR="$HOME_DIR/bin"
REPO="${HOLON_REPO:-markkovari/holon}"
ROLE="${HOLON_ROLE:-worker}"

# `worker` is the set a second machine needs to take gate work. `full` is a box
# that also drives the loop and serves apps.
case "$ROLE" in
  worker) DEFAULT_BINS="comp-checks" ;;
  full)   DEFAULT_BINS="comp-checks comp-host comp-plug comp-field comp-relay comp-goalrun comp-goald holon" ;;
  *)      echo "install.sh: HOLON_ROLE must be 'worker' or 'full', got '$ROLE'" >&2; exit 2 ;;
esac
BINS="${HOLON_BINS:-$DEFAULT_BINS}"

case "$(uname -s)" in
  Darwin) os=darwin ;;
  Linux)  os=linux ;;
  *) echo "install.sh: unsupported OS '$(uname -s)' — macOS and Linux only" >&2; exit 2 ;;
esac
case "$(uname -m)" in
  arm64|aarch64) [ "$os" = darwin ] && arch=arm64 || arch=aarch64 ;;
  x86_64|amd64)  arch=x86_64 ;;
  *) echo "install.sh: unsupported architecture '$(uname -m)'" >&2; exit 2 ;;
esac
TARGET="$os-$arch"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "install.sh: '$1' is required and not on PATH." >&2
    [ "$1" = cargo ] && echo "  install Rust: https://rustup.rs" >&2
    exit 2
  }
}
need tar
mkdir -p "$BIN_DIR"

# --- the source build, for when there is no binary to download ---------------
#
# Five cargo workspaces, not one, so `cargo build --bin comp-host` from the root
# finds nothing.
workspace_of() {
  case "$1" in
    comp-host) echo host ;;
    holon)     echo cli ;;
    *)         echo reconciler ;;
  esac
}

from_source() {
  need cargo
  if [ -n "${HOLON_SRC:-}" ]; then
    src="$HOLON_SRC"
    [ -f "$src/reconciler/Cargo.toml" ] || {
      echo "install.sh: HOLON_SRC='$src' is not a Holon checkout (no reconciler/Cargo.toml)" >&2
      exit 2
    }
  else
    need curl
    src="$(mktemp -d)"
    ref="${HOLON_VERSION:-main}"
    echo ">> source https://github.com/$REPO/archive/$ref.tar.gz"
    # --strip-components=1: the archive's top directory is named after the ref,
    # and a ref with a slash in it ('feat/x') makes that name unpredictable.
    curl -fsSL "https://github.com/$REPO/archive/$ref.tar.gz" \
      | tar -xzf - --strip-components=1 -C "$src" || {
        echo "install.sh: could not fetch or unpack the source for $ref" >&2
        exit 1
      }
  fi
  echo ">> building $BINS (this needs Rust, and takes a few minutes)"
  for bin in $BINS; do
    ws="$(workspace_of "$bin")"
    ( cd "$src/$ws" && cargo build --release --quiet --bin "$bin" ) || {
      echo "install.sh: building $bin (in $ws/) failed" >&2
      exit 1
    }
    cp "$src/$ws/target/release/$bin" "$BIN_DIR/$bin"
  done
}

# --- the download ------------------------------------------------------------
from_release() {
  need curl
  version="${HOLON_VERSION:-}"
  if [ -z "$version" ]; then
    # The latest NON-prerelease. `releases/latest` already excludes pre-releases,
    # which is what keeps a hand-run dev build from becoming what everybody gets
    # by being the most recent thing published.
    version=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null \
      | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)
    [ -n "$version" ] || return 1
  fi
  url="https://github.com/$REPO/releases/download/$version/holon-$ROLE-$TARGET.tar.gz"
  echo ">> $url"
  tmp="$(mktemp -d)"
  curl -fsSL "$url" -o "$tmp/a.tar.gz" 2>/dev/null || { rm -rf "$tmp"; return 1; }
  tar -xzf "$tmp/a.tar.gz" -C "$tmp" || { rm -rf "$tmp"; return 1; }
  for bin in $BINS; do
    [ -f "$tmp/$bin" ] || { rm -rf "$tmp"; return 1; }
    cp "$tmp/$bin" "$BIN_DIR/$bin"
    chmod +x "$BIN_DIR/$bin"
  done
  rm -rf "$tmp"
  echo ">> $version, prebuilt for $TARGET"
}

if [ -n "${HOLON_SRC:-}" ] || [ -n "${HOLON_BINS:-}" ]; then
  from_source
elif ! from_release; then
  echo ">> no prebuilt binary for $TARGET (or no release yet) — building from source instead"
  from_source
fi

# The one runnable check: an installed binary that cannot answer --help was
# copied from the wrong place or built against a libc this machine does not have.
# `GLIBC_2.38 not found` at first use, long after the installer said it worked,
# is the failure this exists to turn into an install-time one.
for bin in $BINS; do
  "$BIN_DIR/$bin" --help >/dev/null 2>&1 || {
    echo "install.sh: $BIN_DIR/$bin was installed but does not run:" >&2
    "$BIN_DIR/$bin" --help >/dev/null || true
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
