#!/usr/bin/env python3
"""What a goal's app is made of, and how much of it the run had to write.

    python3 tools/reuse-ratio.py .comp/goals/triage-assist.toml [...]

Three numbers per app, each from a different source so no one of them can be talked up:

  COMPOSITION   the components `comp-plug` actually wires in, derived from the compiled
                artifact's imports — not from a list anybody maintains by hand.
  CODE          non-comment, non-blank Rust lines in those components' own sources,
                against the lines in the goal's `writable` files. Generated `bindings.rs`
                is excluded from both sides: counting it would flatter the reused side by
                tens of thousands of lines and measure wit-bindgen, not reuse.
  CAPABILITIES  the interfaces the compiled component IMPORTS, against the ones its world
                offers. A world can offer a capability a part never calls; only the
                import proves it was reached for, which is what the gates assert.

The ratio is code-based: reused / (reused + written). It is a ratio of what EXISTS to
what was AUTHORED for this app, which is the question "did the pool carry the weight".
"""
import json
import os
import re
import subprocess
import sys
import tomllib

# `REUSE_ROOT` points this at another checkout — a worktree of a landed pull request,
# which is the only tree where the written side is the run's actual work rather than the
# stubs the repository keeps.
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ROOT = os.environ.get("REUSE_ROOT") or REPO


def sloc(path):
    """Rust lines that are neither blank nor a comment. Crude on purpose: a token-level
    count would be more precise and no more honest, since both sides are counted the
    same way."""
    n = 0
    for line in open(path, errors="ignore"):
        s = line.strip()
        if not s or s.startswith("//") or s.startswith("/*") or s.startswith("*"):
            continue
        n += 1
    return n


def crate_sloc(crate_dir):
    total = 0
    src = os.path.join(crate_dir, "src")
    for base, _, files in os.walk(src):
        for f in files:
            if f.endswith(".rs") and f != "bindings.rs":
                total += sloc(os.path.join(base, f))
    return total


def target_dirs():
    """Where built components are looked for: the measured tree first, the repository
    second. A worktree of a landed branch has the domain artifact and nothing else built,
    and the providers it composes against are the repository's — resolving them from
    there is the same catalogue `comp-plug` uses in a gate (earlier directories win)."""
    dirs = []
    for root in (ROOT, REPO):
        for d in ("wasm32-wasip2", "wasm32-wasip1"):
            path = os.path.join(root, "components/target", d, "debug")
            if os.path.isdir(path):
                dirs += ["--dir", path]
    return dirs


def plugs(crate):
    out = subprocess.run(
        [os.path.join(REPO, "reconciler/target/release/comp-plug"), crate, "--wiring",
         *target_dirs()],
        capture_output=True, text=True, cwd=ROOT,
    ).stdout
    m = re.search(r"plugs: (.*)", out)
    return [p.strip() for p in m.group(1).split(",") if p.strip()] if m else []


def artifact_imports(crate):
    """The interfaces the UNCOMPOSED artifact imports — what the code actually calls."""
    wasm = crate.replace("-", "_") + ".wasm"
    for d in ("wasm32-wasip2", "wasm32-wasip1"):
        path = os.path.join(ROOT, "components/target", d, "debug", wasm)
        if os.path.exists(path):
            wit = subprocess.run(["wasm-tools", "component", "wit", path],
                                 capture_output=True, text=True).stdout
            return sorted({m.group(1) for m in re.finditer(r"import ([a-z0-9:-]+/[a-z0-9-]+)", wit)})
    return []


def world_imports(crate):
    for base, _, files in os.walk(os.path.join(ROOT, "components", crate, "wit")):
        for f in files:
            if f.endswith(".wit"):
                text = open(os.path.join(base, f)).read()
                return sorted({m.group(1) for m in re.finditer(r"import ([a-z0-9:-]+/[a-z0-9-]+)@", text)})
    return []


def report(goal_path):
    goal = tomllib.load(open(goal_path, "rb"))
    written_files = [w for p in goal.get("part", []) for w in p.get("writable", [])
                     if w.endswith(".rs")]
    crate = None
    for w in written_files:
        parts = w.split("/")
        if len(parts) > 2 and parts[0] == "components":
            crate = parts[1]
            break
    if not crate:
        print(f"{goal_path}: no writable .rs under components/ — nothing to measure")
        return None

    written = sum(sloc(os.path.join(ROOT, f)) for f in written_files
                  if os.path.exists(os.path.join(ROOT, f)))
    # A stub tree would report a 99% ratio against sixteen lines of `not_implemented`,
    # which is true and meaningless. Say so rather than print it as a result.
    stubs = [f for f in written_files
             if os.path.exists(os.path.join(ROOT, f))
             and '501, "not_implemented"' in open(os.path.join(ROOT, f), errors="ignore").read()]
    if stubs:
        print(f"\n=== {goal.get('title', goal_path)}")
        print(f"    SKIPPED — {len(stubs)} of {len(written_files)} written file(s) are still"
              f" stubs in this tree.\n    Measure a landed branch instead:"
              f" REUSE_ROOT=<worktree> python3 tools/reuse-ratio.py {goal_path}")
        return None
    router = sloc(os.path.join(ROOT, "components", crate, "src", "lib.rs"))
    wired = plugs(crate)
    reused = sum(crate_sloc(os.path.join(ROOT, "components", c)) for c in wired
                 if os.path.isdir(os.path.join(ROOT, "components", c)))
    offered, called = world_imports(crate), artifact_imports(crate)
    # A world entry is "reached" if the artifact imports the same package/interface.
    reached = [i for i in offered if i in called]

    ratio = reused / (reused + written) if reused + written else 0
    print(f"\n=== {goal.get('title', goal_path)}")
    print(f"    crate: {crate}")
    print(f"  COMPOSITION  {len(wired)} component(s) wired in: {', '.join(wired)}")
    print(f"  CODE         reused {reused} sloc  ·  written {written} sloc"
          f"  ·  scaffold (router) {router} sloc")
    print(f"               reuse ratio {ratio:.1%}  — {reused // max(written, 1)}x more"
          f" existing code than authored")
    print(f"  CAPABILITIES world offers {len(offered)}, artifact imports {len(reached)}:")
    for i in offered:
        print(f"      {'called  ' if i in called else 'UNUSED  '} {i}")
    return {"crate": crate, "wired": wired, "reused": reused, "written": written,
            "router": router, "ratio": ratio, "offered": offered, "reached": reached}


if __name__ == "__main__":
    rows = [r for r in (report(g) for g in sys.argv[1:]) if r]
    if len(rows) > 1:
        tr, tw = sum(r["reused"] for r in rows), sum(r["written"] for r in rows)
        print(f"\n=== all {len(rows)} app(s): reused {tr} sloc, written {tw} sloc,"
              f" ratio {tr / (tr + tw):.1%}")
