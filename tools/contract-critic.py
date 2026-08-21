#!/usr/bin/env python3
"""Check a goal's contract against the capabilities its parts actually import — before a run.

    python3 tools/contract-critic.py .comp/goals/treasury-ledger.toml

The cheap, deterministic half of what a branch did by hand twice in this experiment: it read a
contract, found a claim the components did not support, and filed a CONTRACT-REQUEST — after a
generation had already been spent. This catches the two shapes that cost real runs, with no
model call:

  * a context file that does not resolve (a dangling path is shown to no one and silently drops
    a capability from what the part can see);
  * a capability a part's WORLD imports whose call signature the contract never quotes, so the
    part must guess it — the class that produced app 6's uncovered `idempotency`.

It does NOT read component source to refute a contract's CLAIMS about behaviour — that is the
model-based critic, a larger and non-deterministic thing. This is the part that pays for itself
on every goal for free, and it is worth running before `goal-rehearse.sh`, which is slower.
"""
import re
import sys
import tomllib
from pathlib import Path

# package namespace -> the binding alias a contract quotes it under (e.g. money:amount -> money::)
ALIAS = {
    "records": "records", "money": "money", "ledger": "ledger", "idempotency": "idem",
    "ratelimit": "rl", "pii": "pii", "ai": "ai", "fsm": "fsm", "cache": "cache",
    "quota": "meter", "search": "search", "otp": "totp", "session": "sessions",
    "event": "bus", "policy": "policy", "outbox": "outbox", "notify": "notify",
    "audit": "audit", "auth": "authz", "lock": "mutex",
}


def check(goal_path: str) -> int:
    goal = tomllib.load(open(goal_path, "rb"))
    if "part" not in goal:
        print(f"{goal_path}: single-part goal, nothing to cross-check")
        return 0
    contract = Path(goal["contract"]).read_text()
    problems = []

    # 1. Every context path must resolve.
    for part in goal["part"]:
        for c in part.get("context", []):
            if not Path(c).exists():
                problems.append(f"{part['name']}: context path does not exist: {c}")

    # 2. Every capability a part's world imports should have its signature quoted in the contract.
    #    The world is the app's own .wit in the part's context.
    for part in goal["part"]:
        wits = [c for c in part.get("context", []) if c.endswith(".wit") and "-domain/" in c]
        for w in wits:
            if not Path(w).exists():
                continue
            for m in re.finditer(r"import ([a-z0-9-]+):", Path(w).read_text()):
                ns = m.group(1)
                if ns in ("wasi", "comp"):
                    continue
                alias = ALIAS.get(ns, ns)
                covered = bool(re.search(rf"\b{re.escape(alias)}::", contract)) or f"{ns}:" in contract
                if not covered:
                    problems.append(
                        f"{part['name']}: world imports `{ns}:` but the contract never quotes "
                        f"`{alias}::` — the part must guess the signature"
                    )

    problems = sorted(set(problems))
    title = goal.get("title", goal_path)
    if not problems:
        print(f"OK  {title}")
        return 0
    print(f"FAIL {title}")
    for p in problems:
        print(f"  · {p}")
    return 1


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit("usage: contract-critic.py <goal.toml> [<goal.toml> ...]")
    rc = 0
    for g in sys.argv[1:]:
        rc |= check(g)
    sys.exit(rc)
