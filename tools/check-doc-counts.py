#!/usr/bin/env python3
"""Every number the README states about this tree, checked against the tree.

A count in prose is a claim with no owner. It is right on the day it is typed
and silently wrong forever after, and the reader who notices is the one who
counted — by which point they have stopped believing the rest of the document
too.

So the numbers stay in the README, where they are useful, and this says when
they stop being true. Run it, or let CI run it:

    python3 tools/check-doc-counts.py

Exits non-zero and prints what moved. Adding a new claim means adding a check
here; that is the point, not an obstacle.
"""

from __future__ import annotations

import glob
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def adr_files() -> list[str]:
    """Every decision, excluding the index."""
    return [
        p
        for p in glob.glob(os.path.join(ROOT, "docs/adr/*.md"))
        if not os.path.basename(p).lower().startswith("readme")
    ]


def superseded() -> list[str]:
    """ADRs marked superseded in their own header.

    Deliberately narrow: an ADR that merely mentions the word — usually because
    it is the one doing the superseding — is not itself superseded. Matching on
    the whole file gives 26, and that number would be wrong in a way nobody
    could see.
    """
    out = []
    for p in adr_files():
        head = open(p, encoding="utf-8", errors="ignore").read()[:2000]
        if re.search(r"^\s*(?:status|>)\s*.{0,40}supersed", head, re.I | re.M):
            out.append(p)
    return out


def app_docs() -> list[str]:
    return [
        p
        for p in glob.glob(os.path.join(ROOT, "docs/apps/*.md"))
        if not os.path.basename(p).lower().startswith("readme")
    ]


def component_crates() -> list[str]:
    d = os.path.join(ROOT, "components")
    return [c for c in os.listdir(d) if os.path.exists(os.path.join(d, c, "Cargo.toml"))]


def readme() -> str:
    return open(os.path.join(ROOT, "README.md"), encoding="utf-8").read()


def main() -> int:
    text = readme()
    failures: list[str] = []

    def claim(label: str, pattern: str, actual: int) -> None:
        """The README must state `actual` where `pattern` captures a number."""
        m = re.search(pattern, text)
        if not m:
            failures.append(f"{label}: no claim matching /{pattern}/ found in README.md")
            return
        stated = int(m.group(1))
        if stated != actual:
            failures.append(f"{label}: README says {stated}, tree has {actual}")

    claim("ADRs", r"the reasoning — (\d+) decisions", len(adr_files()))
    claim("superseded ADRs", r"decisions, (\d+) of them superseded", len(superseded()))
    claim("showcase apps", r"the (\d+) showcase apps", len(app_docs()))
    claim("components (ADR-0089 row)", r"— (\d+) components, reuse enforced", len(component_crates()))

    if failures:
        print("doc counts have drifted:\n")
        for f in failures:
            print(f"  {f}")
        print("\nFix the README, or fix the tree. Both are legitimate; leaving them")
        print("disagreeing is not.")
        return 1

    print(
        f"doc counts agree with the tree: {len(adr_files())} ADRs "
        f"({len(superseded())} superseded), {len(app_docs())} app docs, "
        f"{len(component_crates())} components"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
