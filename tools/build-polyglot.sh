#!/usr/bin/env bash
# Build one of the binder's capabilities in a language that is not Rust.
#
#   tools/build-polyglot.sh <lang> <capability>
#
#   go   portfolio-value | price-history
#   c    portfolio-value
#   js   price-history
#   py   price-history
#
# Rust components here are built by `cargo component`, which does embed-and-encode
# in one step. Nothing else does, so this is that step written out per language —
# and the differences between the four are the interesting part, not an accident of
# how this script is organised. See components/portfolio-value-go/README.md.
#
# Output goes to components/target/<capability>_<lang>.wasm. `just e2e-binder-poly`
# swaps one in for the Rust build and runs the app's own e2e against it.
set -euo pipefail
cd "$(dirname "$0")/.." || exit 1
root="$PWD"
out_dir="$root/components/target"

lang="${1:?usage: build-polyglot.sh <go|c|js|py> <portfolio-value|price-history>}"
cap="${2:?}"
snake="${cap//-/_}"
out="$out_dir/${snake}_${lang}.wasm"
mkdir -p "$out_dir"

# wasi-sdk, for C. Not vendored: it is a 200 MB toolchain and this is the only
# thing in the repository that needs it.
wasi_sdk="${WASI_SDK_PATH:-$HOME/.local/share/wasi-sdk-25.0}"

case "$lang" in
go)
  # Go targets wasip1; the component model wants wasip2, so this is
  # compile -> embed the world -> lift through an adapter.
  #
  # Two things here are the finding, not a workaround:
  #
  #   -ldflags=-checklinkname=0
  #       `go.bytecodealliance.org/pkg/wit/runtime` reaches `runtime.sbrk` by
  #       linkname to allocate the canonical ABI's return area, and Go 1.23 made
  #       that an error. Without this the link fails outright.
  #
  #   the adapter's vintage
  #       It must be roughly the same age as wasm-tools. The one vendored under
  #       bench/node_modules with jco is older than ours and is rejected for a
  #       missing export, so a current one is fetched once and cached.
  #
  # TinyGo would target wasip2 natively and need no adapter, and would be far
  # smaller — but it cannot build these bindings at all: no `runtime.Pinner`.
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
  # The only one that needs no post-processing: wasi-sdk's clang targets
  # wasm32-wasip2 and its `wasm-component-ld` emits a component directly.
  #
  # `-mexec-model=reactor` is not cosmetic. Without it the artifact is a COMMAND:
  # it exports `wasi:cli/run` and imports an exit, an environment and stderr for a
  # `main` that is never called. As a reactor the import section is empty, which is
  # what the WIT world actually says.
  [ -x "$wasi_sdk/bin/clang" ] || {
    echo "no wasi-sdk at '$wasi_sdk' — set WASI_SDK_PATH." >&2
    echo "  https://github.com/WebAssembly/wasi-sdk/releases" >&2
    exit 1
  }
  cd "components/${cap}-c"
  # The component-type object is a binary wit-bindgen derives from the WIT, so it
  # is regenerated here rather than committed. The .c/.h beside it ARE committed:
  # they are the canonical ABI in readable form.
  wit-bindgen c --world "$cap" --out-dir . ./wit >/dev/null
  "$wasi_sdk/bin/clang" --target=wasm32-wasip2 -mexec-model=reactor -O2 -Wall -Wextra \
    -o "$out" ./*.c ./*_component_type.o
  ;;

js)
  # `componentize-js`, already vendored under bench/node_modules — the only one of
  # the four that needed nothing installed. SpiderMonkey ends up inside the
  # artifact, which is where the 12 MB comes from.
  jco="$root/bench/node_modules/.bin/jco"
  [ -x "$jco" ] || { echo "no jco at '$jco' — npm ci in bench/" >&2; exit 1; }
  src=$(echo "components/${cap}-js"/*.js)
  "$jco" componentize "$src" \
    --wit "components/${cap}/wit" --world-name "$cap" -o "$out"
  ;;

py)
  # `componentize-py` is the odd one out: it does not use wit-bindgen at all, it
  # has its own generator, and it ships a CPython interpreter plus this module's
  # bytecode inside the artifact. What runs in the composition is an interpreter.
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
