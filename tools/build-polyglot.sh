#!/usr/bin/env bash
# Build a binder capability in Go, C, JS or Python: the embed-and-encode `cargo component` does for Rust,
# written out per language (components/portfolio-value-go/README.md).
set -euo pipefail
cd "$(dirname "$0")/.." || exit 1
root="$PWD"
out_dir="$root/components/target"

lang="${1:?usage: build-polyglot.sh <go|c|js|py> <portfolio-value|price-history>}"
cap="${2:?}"
snake="${cap//-/_}"
out="$out_dir/${snake}_${lang}.wasm"
mkdir -p "$out_dir"

# Not vendored: a 200 MB toolchain only this script needs.
wasi_sdk="${WASI_SDK_PATH:-$HOME/.local/share/wasi-sdk-25.0}"

case "$lang" in
go)
  # -checklinkname=0: the wit runtime linknames `runtime.sbrk`, an error since Go 1.23.
  # The adapter must match wasm-tools' age; jco's vendored one is too old. TinyGo lacks `runtime.Pinner`.
  adapter="$out_dir/wasi_snapshot_preview1.reactor.wasm"
  if [ ! -f "$adapter" ]; then
    echo "fetching the wasip1->wasip2 adapter once into components/target/"
    curl -sSL --fail -o "$adapter" \
      https://github.com/bytecodealliance/wasmtime/releases/latest/download/wasi_snapshot_preview1.reactor.wasm
  fi
  cd "components/${cap}-go"
  core="$(mktemp -t polycore).wasm"
  embedded="$(mktemp -t polyembed).wasm"
  trap 'rm -f "$core" "$embedded"' EXIT
  GOOS=wasip1 GOARCH=wasm go build -buildmode=c-shared -ldflags=-checklinkname=0 -o "$core" .
  wasm-tools component embed ./wit "$core" --world "${cap}-go" -o "$embedded"
  wasm-tools component new "$embedded" --adapt "wasi_snapshot_preview1=$adapter" -o "$out"
  ;;

c)
  # `-mexec-model=reactor`: as a command it would import exit/env/stderr for a `main` never called.
  [ -x "$wasi_sdk/bin/clang" ] || {
    echo "no wasi-sdk at '$wasi_sdk' — set WASI_SDK_PATH." >&2
    echo "  https://github.com/WebAssembly/wasi-sdk/releases" >&2
    exit 1
  }
  cd "components/${cap}-c"
  # The component-type .o is regenerated, not committed; the .c/.h are committed as readable ABI.
  wit-bindgen c --world "$cap" --out-dir . ./wit >/dev/null
  "$wasi_sdk/bin/clang" --target=wasm32-wasip2 -mexec-model=reactor -O2 -Wall -Wextra \
    -o "$out" ./*.c ./*_component_type.o
  ;;

js)
  jco="$root/bench/node_modules/.bin/jco"
  [ -x "$jco" ] || { echo "no jco at '$jco' — npm ci in bench/" >&2; exit 1; }
  src=$(echo "components/${cap}-js"/*.js)
  "$jco" componentize "$src" \
    --wit "components/${cap}/wit" --world-name "$cap" -o "$out"
  ;;

py)
  command -v componentize-py >/dev/null || {
    echo "no componentize-py — 'uv tool install componentize-py'" >&2
    exit 1
  }
  cd "components/${cap}-py"
  componentize-py --wit-path "../${cap}/wit" --world "$cap" componentize app -o "$out"
  ;;

*)
  echo "unknown language '$lang' (go, c, js, py)" >&2
  exit 1
  ;;
esac

imports=$(wasm-tools component wit "$out" | sed -n '/^world root/,/^}/p' | grep -c '^  import ' || true)
printf '%s in %s -> %s (%s KB, %s imports)\n' "$cap" "$lang" "${out#"$root"/}" \
  "$(( $(wc -c < "$out") / 1024 ))" "$imports"
