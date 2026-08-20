#!/usr/bin/env bash
# Rehearse a goal's gates in a reconstruction of the sandbox the loop will run them in.
#
#   bash tools/goal-rehearse.sh .comp/goals/triage-assist.toml            # must FAIL
#   REF=/path/to/reference bash tools/goal-rehearse.sh .comp/goals/…      # must PASS
#
# WHY THIS EXISTS. A gate that passes in the repository can still fail in the loop, and
# it fails identically for every branch: the sandbox holds only the goal's `base_paths`
# with `keep_members` applied, and anything the build reaches for outside that list is
# simply absent. Run 1 of docs/measure/complex-five.md lost 36 model calls to one
# missing directory — `components/llm-inference/`, needed by a WIT dependency path in a
# manifest, listed nowhere. Every branch was told its part did not compile, several
# spent their generation arguing with the contract about a Cargo.toml that was right
# there, and the loop's own base-tree precheck reported all four checks failing as
# healthy, because a check that fails for the WRONG reason still fails.
#
# So this rebuilds that tree and runs the goal's checks under the same cleared
# environment `comp-checks` uses. Twice, and both directions matter:
#
#   without REF  every check must fail, and the reason must be the part being
#                unimplemented — not a build error
#   with REF     every check must pass, which is the only evidence the gates are
#                passable at all in that environment
set -uo pipefail
GOAL="${1:?usage: goal-rehearse.sh <goal.toml> (REF=<dir with the part files> to rehearse a passing tree)}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d -t goal-rehearse-XXXX)"
trap 'rm -rf "$WORK"' EXIT

cd "$ROOT" || exit 1
python3 - "$GOAL" "$WORK" <<'PY' || exit 1
import os, re, shutil, sys, tomllib
goal, dst = sys.argv[1], sys.argv[2]
d = tomllib.load(open(goal, 'rb'))
for p in d.get('base_paths', []):
    src = p.rstrip('/')
    if not os.path.exists(src):
        print(f"base path does not exist in the repository: {src}")
        sys.exit(1)
    target = os.path.join(dst, src)
    os.makedirs(os.path.dirname(target), exist_ok=True)
    shutil.copytree(src, target, dirs_exist_ok=True) if os.path.isdir(src) else shutil.copy2(src, target)
# `keep_members` as the loop rewrites it: a member whose directory was not copied is a
# workspace that cannot be loaded, which is its own way to fail every branch at once.
manifest = d.get('workspace_manifest')
keep = d.get('keep_members')
if manifest and keep:
    path = os.path.join(dst, manifest)
    text = open(path).read()
    members = ', '.join(f'"{k}"' for k in keep)
    open(path, 'w').write(re.sub(r'members = \[[^\]]*\]', f'members = [{members}]', text, count=1))
checks = [c['command'] for c in d.get('check', [])]
for part in d.get('part', []):
    checks += [c['command'] for c in part.get('check', [])]
# Trailing newline, because `while read` drops a final line without one and the check
# it names would never run — a rehearsal quietly less thorough than it says it is.
open(os.path.join(dst, '.rehearse-checks'), 'w').write(
    ''.join(' '.join(c) + '\n' for c in checks))
print(f"sandbox: {len(d.get('base_paths', []))} base path(s), {len(checks)} check(s)")
PY

# The part files a branch would have written. Without them the tree is the base tree.
if [ -n "${REF:-}" ]; then
  for f in "$REF"/*.rs; do
    [ -e "$f" ] || continue
    dest=$(find "$WORK" -type d -name src -path '*-domain*' | head -1)
    cp "$f" "$dest/$(basename "$f")"
  done
  echo "reference applied from $REF"
fi

# What `comp-checks` passes a check, and nothing else. `HOME` is the sandbox, so a
# check that reaches into the real home is a check that will not work in the loop.
TOOLCHAIN=$(rustup show active-toolchain 2>/dev/null | cut -d' ' -f1)
failed=0
cd "$WORK" || exit 1
while read -r cmd; do
  [ -n "$cmd" ] || continue
  echo "--- $cmd"
  env -i PATH="$PATH" HOME="$WORK" CARGO_TERM_COLOR=never \
    RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" RUSTUP_TOOLCHAIN="$TOOLCHAIN" \
    CARGO_HOME="$HOME/.cache/comp-goalrun/cargo-home" \
    CARGO_TARGET_DIR="$HOME/.cache/comp-goalrun/cargo-target" \
    COMP_HOST="$ROOT/host/target/release/comp-host" \
    COMP_PLUG="$ROOT/reconciler/target/release/comp-plug" \
    bash -c "$cmd" 2>&1 | tail -3
  status=${PIPESTATUS[0]}
  [ "$status" = 0 ] || failed=$((failed + 1))
  echo "    exit $status"
done < .rehearse-checks

echo
if [ -n "${REF:-}" ]; then
  [ "$failed" = 0 ] && echo "REHEARSAL OK — every check passes on a reference tree" && exit 0
  echo "REHEARSAL FAILED — $failed check(s) cannot pass even with the reference: the gates are not passable in the sandbox"
  exit 1
fi
# A build error here is the failure this script exists for: it fails every branch
# identically, and the loop cannot tell it apart from an unimplemented part.
echo "$failed check(s) failed on the base tree, which is what should happen."
echo "Read the reasons above: a compile or compose error is a BROKEN SANDBOX, not a"
echo "judgeable check. Only 'never calls …' / behavioural failures mean the tree is right."
