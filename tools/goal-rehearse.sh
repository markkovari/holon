#!/usr/bin/env bash
# Rehearse a goal's gates in a rebuilt loop sandbox: without REF every check must fail, with REF every
# check must pass (and each part alone), and each VIOLATORS=<dir>/* must be rejected by its `must-fail` check.
set -uo pipefail
GOAL="${1:?usage: goal-rehearse.sh <goal.toml> (REF=<dir with the part files> to rehearse a passing tree)}"
# Absolute: the checks run from inside the sandbox, where a relative path stops resolving.
case "$GOAL" in /*) ;; *) GOAL="$PWD/$GOAL" ;; esac
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d -t goal-rehearse-XXXX)"
trap 'rm -rf "$WORK"' EXIT

cd "$ROOT" || exit 1
python3 - "$GOAL" "$WORK" <<'PY' || exit 1
import os, re, shutil, sys, tomllib
goal, dst = sys.argv[1], sys.argv[2]
d = tomllib.load(open(goal, 'rb'))

# `component = "<name>"` derives the build scope in goalrun (component_scope()), so a
# goal that uses the shorthand lists no base_paths at all — and this script copied
# nothing, ran the checks against an EMPTY sandbox, and reported "1 check(s) failed on
# the base tree, which is what should happen". Every check fails when the tree is
# empty. The guard against a broken sandbox was itself the broken sandbox.
#
# Derived identically to goalrun so the two cannot disagree; an explicitly-set field
# still wins, which is that function's rule too.
component = d.get('component')
if component:
    d.setdefault('base_paths', [f'components/{component}/', 'components/Cargo.toml'])
    d.setdefault('workspace_manifest', 'components/Cargo.toml')
    d.setdefault('keep_members', [component])

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
    # `[^\]]*` stopped at the first `]`, and in components/Cargo.toml that one is
    # inside a comment (`… declares its own `[workspace]`, so …`). The rewrite then
    # produced invalid TOML and every check failed to build, which this script
    # reported as a healthy base tree. Walk it instead, honouring `#` and quotes —
    # the same fix as goalrun's `closing_bracket`.
    open_at = text.index('[', text.index('members'))
    i, in_comment, in_string = open_at + 1, False, False
    while i < len(text):
        c = text[i]
        if c == '\n':
            in_comment = False
        elif c == '"' and not in_comment:
            in_string = not in_string
        elif c == '#' and not in_string:
            in_comment = True
        elif c == ']' and not in_comment and not in_string:
            break
        i += 1
    else:
        print(f"never found the end of the members list in {manifest}")
        sys.exit(1)
    text = f'{text[:open_at]}[{members}]{text[i + 1:]}'
    open(path, 'w').write(text)
    try:
        tomllib.loads(text)
    except Exception as e:
        print(f"the trimmed {manifest} is not valid TOML, so every check would fail to build: {e}")
        sys.exit(1)
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

# Kept before REF is applied: isolation starts from every part a stub.
BASE="$(mktemp -d -t goal-base-XXXX)"
cp -R "$WORK"/. "$BASE"/
trap 'rm -rf "$WORK" "$BASE"' EXIT

if [ -n "${REF:-}" ]; then
  for f in "$REF"/*.rs; do
    [ -e "$f" ] || continue
    dest=$(find "$WORK" -type d -name src -path '*-domain*' | head -1)
    cp "$f" "$dest/$(basename "$f")"
  done
  echo "reference applied from $REF"
fi

# The env `comp-checks` gives a check, except an own CARGO_TARGET_DIR so a live run's builds don't collide.
crashed_check() { # crashed_check <output-file>
  grep -q "Traceback (most recent call last)" "$1" || return 1
  # Only the LAST exception counts: a guard re-raising as AssertionError prints a chained one first.
  local last
  last=$(grep -oE "^[A-Za-z_][A-Za-z0-9_.]*(Error|Exception):" "$1" | tail -1)
  [ -n "$last" ] && [ "$last" != "AssertionError:" ]
}

TOOLCHAIN=$(rustup show active-toolchain 2>/dev/null | cut -d' ' -f1)
failed=0
crashed=0
cd "$WORK" || exit 1
while read -r cmd; do
  [ -n "$cmd" ] || continue
  echo "--- $cmd"
  out="$(mktemp -t goal-rehearse-out-XXXX)"
  env -i PATH="$PATH" HOME="$WORK" CARGO_TERM_COLOR=never \
    RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" RUSTUP_TOOLCHAIN="$TOOLCHAIN" \
    CARGO_HOME="$HOME/.cache/comp-goalrun/cargo-home" \
    CARGO_TARGET_DIR="$HOME/.cache/goal-rehearse/cargo-target" \
    COMP_HOST="$ROOT/host/target/release/comp-host" \
    COMP_PLUG="$ROOT/reconciler/target/release/comp-plug" \
    bash -c "$cmd" >"$out" 2>&1
  status=$?
  tail -3 "$out"
  # 127/126: the gate never ran, yet downstream it looks exactly like a failing check.
  if [ "$status" = 127 ] || [ "$status" = 126 ]; then
    echo "    CANNOT RUN (exit $status) — the shell never reached this gate."
    echo "    Either the script is not in the sandbox (an untracked file is not in the base"
    echo "    tree; \`git add\` it, or widen base_paths) or it is not executable."
    crashed=$((crashed + 1))
  elif crashed_check "$out"; then
    echo "    CRASHED — this check raised instead of failing with a sentence."
    echo "    Whatever a branch did, the feedback it gets is a stack trace. Guard the parse"
    echo "    and say what was wrong with what the component answered."
    crashed=$((crashed + 1))
  fi
  rm -f "$out"
  [ "$status" = 0 ] || failed=$((failed + 1))
  echo "    exit $status"
done < .rehearse-checks

# The loop judges each part with its siblings stubbed.
if [ -n "${REF:-}" ]; then
  echo
  lonely=0
  python3 - "$GOAL" "$WORK/.rehearse-parts" <<'PY'
import sys, tomllib
goal = tomllib.load(open(sys.argv[1], 'rb'))
rows = []
for part in goal.get('part', []):
    files = [w for w in part.get('writable', []) if w.endswith('.rs')]
    checks = [' '.join(c['command']) for c in part.get('check', [])]
    if files and checks:
        rows.append('\t'.join([part['name'], ','.join(files), checks[0]]))
open(sys.argv[2], 'w').write(''.join(r + '\n' for r in rows))
PY
  if [ ! -s "$WORK/.rehearse-parts" ]; then
    echo "could not work out which reference file belongs to which part — the goal's"
    echo "\`writable\` and \`[[part.check]]\` entries are what this reads, and isolation is the"
    echo "direction that catches a part gate depending on a route a sibling owns. Not skipping it."
    exit 1
  fi
  while IFS=$'\t' read -r name files check; do
    [ -n "$name" ] || continue
    tree="$(mktemp -d -t goal-isolate-XXXX)"
    cp -R "$BASE"/. "$tree"/
    dest=$(find "$tree" -type d -name src -path '*-domain*' | head -1)
    for f in ${files//,/ }; do
      base=$(basename "$f")
      [ -f "$REF/$base" ] && cp "$REF/$base" "$dest/$base"
    done
    echo "--- $name alone (siblings stubbed): $check"
    iout="$(mktemp -t goal-isolate-out-XXXX)"
    (
      cd "$tree" || exit 1
      env -i PATH="$PATH" HOME="$tree" CARGO_TERM_COLOR=never \
        RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" RUSTUP_TOOLCHAIN="$TOOLCHAIN" \
        CARGO_HOME="$HOME/.cache/comp-goalrun/cargo-home" \
        CARGO_TARGET_DIR="$HOME/.cache/goal-rehearse/cargo-target" \
        COMP_HOST="$ROOT/host/target/release/comp-host" \
        COMP_PLUG="$ROOT/reconciler/target/release/comp-plug" \
        bash -c "$check" >"$iout" 2>&1
    )
    if [ $? -eq 0 ]; then
      echo "    passes alone, as it must"
    else
      tail -2 "$iout"
      echo "    CANNOT PASS ALONE — this part is judged with its siblings stubbed, so a check"
      echo "    that needs a route another part owns is unsatisfiable by any implementation."
      lonely=$((lonely + 1))
    fi
    rm -rf "$tree" "$iout"
  done < "$WORK/.rehearse-parts"
  if [ "$lonely" -gt 0 ]; then
    echo
    echo "REHEARSAL FAILED — $lonely part(s) cannot pass their own check in isolation."
    exit 1
  fi
fi

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
    # A violator is applied on top of REF.
    tree="$(mktemp -d -t goal-violator-XXXX)"
    cp -R "$WORK"/. "$tree"/
    dest=$(find "$tree" -type d -name src -path '*-domain*' | head -1)
    for f in "$dir"*.rs; do
      [ -e "$f" ] || continue
      cp "$f" "$dest/$(basename "$f")"
    done
    check=$(head -1 "$want")
    vout="$(mktemp -t goal-violator-out-XXXX)"
    echo "--- violator $name  (must be rejected by: $check)"
    (
      cd "$tree" || exit 1
      env -i PATH="$PATH" HOME="$tree" CARGO_TERM_COLOR=never \
        RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" RUSTUP_TOOLCHAIN="$TOOLCHAIN" \
        CARGO_HOME="$HOME/.cache/comp-goalrun/cargo-home" \
        CARGO_TARGET_DIR="$HOME/.cache/goal-rehearse/cargo-target" \
        COMP_HOST="$ROOT/host/target/release/comp-host" \
        COMP_PLUG="$ROOT/reconciler/target/release/comp-plug" \
        bash -c "$check" >"$vout" 2>&1
    )
    vstatus=$?
    if [ "$vstatus" -eq 0 ]; then
      echo "    SURVIVED — the gate accepted an implementation that breaks this rule."
      echo "    The rule is either untestable by that check or not true of the components"
      echo "    involved. Read $dir and decide which."
      broken=$((broken + 1))
    elif crashed_check "$vout"; then
      echo "    CRASHED — the check raised rather than rejecting it, so it did not judge"
      echo "    this violation; it only happened to exit non-zero. Guard the parse first."
      broken=$((broken + 1))
    else
      echo "    rejected, as it must be"
    fi
    rm -f "$vout"
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
  if [ "$crashed" -gt 0 ]; then
    echo "REHEARSAL FAILED — $crashed check(s) crashed rather than judging"
    exit 1
  fi
  [ "$failed" = 0 ] && echo "REHEARSAL OK — every check passes on a reference tree${VIOLATORS:+, and every violator was rejected}" && exit 0
  echo "REHEARSAL FAILED — $failed check(s) cannot pass even with the reference: the gates are not passable in the sandbox"
  exit 1
fi
if [ "$crashed" -gt 0 ]; then
  echo "REHEARSAL FAILED — $crashed check(s) crashed rather than failing cleanly on the base"
  echo "tree. A branch would receive that stack trace as its only feedback."
  exit 1
fi
echo "$failed check(s) failed on the base tree, which is what should happen."
echo "Read the reasons above: a compile or compose error is a BROKEN SANDBOX, not a"
echo "judgeable check. Only 'never calls …' / behavioural failures mean the tree is right."
