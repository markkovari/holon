#!/usr/bin/env python3
"""No two .wit files may define the same package differently.

A package name is a GLOBAL identifier in the component model: `ai:inference@0.1.0`
means one thing, everywhere, forever. Two files declaring it with different
interfaces is not duplication, it is a collision, and it stays invisible for
exactly as long as every consumer happens to name a path instead of the name.

That is not hypothetical — it is what this check was written after. `llm-local`
and `ai-inference` both declared `ai:inference@0.1.0`, with `infer(string) ->
string` behind one and six verbs behind the other. Three domains resolved the
name to one directory and a fourth to the other, and the tree built.

Identical copies are allowed and reported separately. Three components
implementing one contract is the design; the copies are a drift risk rather than
a fault, and this prints them so the risk has a number.

    python3 tools/check-wit-packages.py

Exits non-zero only on a genuine collision.
"""

from __future__ import annotations

import collections
import hashlib
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PACKAGE = re.compile(r"^\s*package\s+([\w:-]+@[\d.]+)\s*;", re.M)


def wit_files() -> list[str]:
    out = []
    for base, dirs, files in os.walk(ROOT):
        # `target/` holds generated copies of the very files being compared.
        dirs[:] = [d for d in dirs if d not in {"target", "node_modules", ".git"}]
        out.extend(os.path.join(base, f) for f in files if f.endswith(".wit"))
    return sorted(out)


def body_after_package(text: str) -> str:
    """Everything the package declares, normalised.

    Comments and whitespace come out: two files that differ only in how they
    explain themselves are the same contract, and flagging that would train
    everyone to ignore this.
    """
    cut = PACKAGE.search(text)
    body = text[cut.end() :] if cut else text
    body = re.sub(r"//[^\n]*", "", body)
    return re.sub(r"\s+", " ", body).strip()


def main() -> int:
    by_package: dict[str, list[tuple[str, str]]] = collections.defaultdict(list)
    for path in wit_files():
        text = open(path, encoding="utf-8", errors="ignore").read()
        for name in PACKAGE.findall(text):
            digest = hashlib.sha256(body_after_package(text).encode()).hexdigest()[:12]
            by_package[name].append((os.path.relpath(path, ROOT), digest))

    collisions, copies = [], []
    for name, entries in sorted(by_package.items()):
        shapes = {d for _, d in entries}
        if len(shapes) > 1:
            collisions.append((name, entries))
        elif len(entries) > 1:
            copies.append((name, entries))

    for name, entries in copies:
        print(f"note: {name} is defined identically in {len(entries)} files")
        for path, _ in entries:
            print(f"        {path}")

    if not collisions:
        print(f"\n{len(by_package)} packages across {len(wit_files())} .wit files, no collisions")
        return 0

    print("\nWIT PACKAGE COLLISION — one name, more than one contract:\n")
    for name, entries in collisions:
        print(f"  {name}")
        for path, digest in entries:
            print(f"      {digest}  {path}")
        print()
    print("A package name is global. Rename one of them, or make them the same file.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
