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
#
# And a third direction, which is the one that matters most:
#
#   VIOLATORS=<dir>   every subdirectory of <dir> is an implementation that BREAKS one
#                     stated rule on purpose, and the check named in its `must-fail` file
#                     must reject it.
#
# WHY THAT THIRD DIRECTION EXISTS. App 4's contract claimed `notify::send` answers
# `Ok(500)` when the far end refuses, and built the courier's central rule on it. The
# component does the opposite (`Err(DeliveryFailed)`, notify-dispatch/src/lib.rs:142), so
# the rule described a component that does not exist — and neither the stub direction nor
# the reference direction could tell: the reference was correct under EITHER reading, so
# the gate never distinguished anything. Four rehearsals passed. One run of six agents read
# the component's own doc comment and filed a contract request.
#
# A violator is how a rule becomes falsifiable. If a violator passes, the rule it breaks is
# either untestable by this gate or not true of the components involved, and both of those
# are worth knowing before a run rather than after it.
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
# Every WIT dependency path named by a crate the workspace keeps must exist in the
# sandbox. This is the omission that cost app 1 its first run — `cargo component` cannot
# build a target world without them, so every branch fails identically, and the message
# names the manifest rather than the missing directory. Checked here statically, by
# reading the manifests rather than by discovering it in a build.
missing_dep = []
for member in keep or []:
    manifest = os.path.join('components', member, 'Cargo.toml')
    if not os.path.exists(manifest):
        continue
    try:
        m = tomllib.load(open(manifest, 'rb'))
        deps = m['package']['metadata']['component']['target']['dependencies']
    except (KeyError, tomllib.TOMLDecodeError):
        continue
    for pkg, spec in deps.items():
        path = spec.get('path') if isinstance(spec, dict) else None
        if not path:
            continue
        rel = os.path.normpath(os.path.join('components', member, path))
        if not os.path.exists(os.path.join(dst, rel)):
            missing_dep.append(f"    {member} needs {pkg} at {rel}")
if missing_dep:
    print("base_paths does not cover every WIT dependency these crates name:")
    print('\n'.join(sorted(set(missing_dep))))
    print("  Every branch would fail with `failed to create a target world`, which names")
    print("  the manifest and not the missing directory. Add the directories to base_paths.")
    sys.exit(1)

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

# What `comp-checks` passes a check, and nothing else — with ONE deliberate difference: a
# target directory of its own. A live run builds the same crate name into
# `comp-goalrun/cargo-target`, and two processes writing one artifact means each is testing
# whatever the other wrote last. `CARGO_HOME` is still shared, because a package registry is
# read-only to both. `HOME` is the sandbox, so a
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
    CARGO_TARGET_DIR="$HOME/.cache/goal-rehearse/cargo-target" \
    COMP_HOST="$ROOT/host/target/release/comp-host" \
    COMP_PLUG="$ROOT/reconciler/target/release/comp-plug" \
    bash -c "$cmd" 2>&1 | tail -3
  status=${PIPESTATUS[0]}
  [ "$status" = 0 ] || failed=$((failed + 1))
  echo "    exit $status"
done < .rehearse-checks

# --- violators: rules that must be enforceable ------------------------------------
if [ -n "${VIOLATORS:-}" ]; then
  echo
  broken=0
  for dir in "$VIOLATORS"/*/; do
    [ -d "$dir" ] || continue
    name=$(basename "$dir")
    want="$dir/must-fail"
    if [ ! -f "$want" ]; then
      echo "SKIP $name — no \`must-fail\` file naming the check that should reject it"
      continue
    fi
    # A violator is the reference with one rule broken, so it is applied ON TOP of REF.
    tree="$(mktemp -d -t goal-violator-XXXX)"
    cp -R "$WORK"/. "$tree"/
    dest=$(find "$tree" -type d -name src -path '*-domain*' | head -1)
    for f in "$dir"*.rs; do
      [ -e "$f" ] || continue
      cp "$f" "$dest/$(basename "$f")"
    done
    check=$(head -1 "$want")
    echo "--- violator $name  (must be rejected by: $check)"
    (
      cd "$tree" || exit 1
      env -i PATH="$PATH" HOME="$tree" CARGO_TERM_COLOR=never \
        RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" RUSTUP_TOOLCHAIN="$TOOLCHAIN" \
        CARGO_HOME="$HOME/.cache/comp-goalrun/cargo-home" \
        CARGO_TARGET_DIR="$HOME/.cache/goal-rehearse/cargo-target" \
        COMP_HOST="$ROOT/host/target/release/comp-host" \
        COMP_PLUG="$ROOT/reconciler/target/release/comp-plug" \
        bash -c "$check" >/dev/null 2>&1
    )
    if [ $? -eq 0 ]; then
      echo "    SURVIVED — the gate accepted an implementation that breaks this rule."
      echo "    The rule is either untestable by that check or not true of the components"
      echo "    involved. Read $dir and decide which."
      broken=$((broken + 1))
    else
      echo "    rejected, as it must be"
    fi
    rm -rf "$tree"
  done
  if [ "$broken" -gt 0 ]; then
    echo
    echo "VIOLATORS SURVIVED: $broken rule(s) are stated and not enforced."
    exit 1
  fi
fi

echo
if [ -n "${REF:-}" ]; then
  [ "$failed" = 0 ] && echo "REHEARSAL OK — every check passes on a reference tree${VIOLATORS:+, and every violator was rejected}" && exit 0
  echo "REHEARSAL FAILED — $failed check(s) cannot pass even with the reference: the gates are not passable in the sandbox"
  exit 1
fi
# A build error here is the failure this script exists for: it fails every branch
# identically, and the loop cannot tell it apart from an unimplemented part.
echo "$failed check(s) failed on the base tree, which is what should happen."
echo "Read the reasons above: a compile or compose error is a BROKEN SANDBOX, not a"
echo "judgeable check. Only 'never calls …' / behavioural failures mean the tree is right."
