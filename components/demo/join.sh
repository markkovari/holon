#!/usr/bin/env bash
# The composition gate for the two-part goal (ADR-0086).
#
# The probe half cannot be judged alone: `cargo component test` runs a crate AS a
# component, and the probe imports `demo:shape/pager`, which nothing satisfies
# standalone — "a matching implementation was not found in the linker". So the
# half that imports the other half is judged HERE, by actually joining them.
#
# What this proves that neither part's own gate can:
#   · the probe's import is satisfied by the component's export — same interface,
#     same version, same types;
#   · the joined thing is a valid component;
#   · it still exports the HTTP handler, so the probe did not quietly stop being
#     a component (`cargo component check` and `build` both pass when it does —
#     measured, which is why this file exists);
#   · and it imports `demo:shape` no longer, which is what "plugged" means.
set -euo pipefail

cargo component build --target wasm32-wasip2 -p demo -p demo-probe \
  --manifest-path components/Cargo.toml

# cargo-component emits under wasip1 even when asked for wasip2; take whichever is
# there rather than guessing, because a gate that fails on a path is a gate that
# says nothing about the code.
T="${CARGO_TARGET_DIR:-components/target}"
for d in wasm32-wasip2 wasm32-wasip1; do
  if [ -f "$T/$d/debug/demo_probe.wasm" ]; then OUT="$T/$d/debug"; break; fi
done
[ -n "${OUT:-}" ] || { echo "no built components under $T"; exit 1; }

JOINED="$(mktemp -t demo-joined-XXXX).wasm"
trap 'rm -f "$JOINED"' EXIT
wac plug "$OUT/demo_probe.wasm" --plug "$OUT/demo.wasm" -o "$JOINED"
wasm-tools validate "$JOINED"

WIT="$(wasm-tools component wit "$JOINED")"
echo "$WIT" | grep -q 'export wasi:http' || {
  echo "the joined component does not export wasi:http — the probe stopped being a component"
  exit 1
}
if echo "$WIT" | grep -q 'import demo:shape'; then
  echo "the joined component still imports demo:shape — the halves did not plug together"
  exit 1
fi
echo "joined: the probe's import is satisfied by the component's export"
