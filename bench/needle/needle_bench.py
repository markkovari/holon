"""Scores Needle 2 on the same cases the shipped matcher is scored on.

    uv venv nv && VIRTUAL_ENV=$PWD/nv uv pip install cactus-needle
    ./nv/bin/python bench/needle/needle_bench.py

Writes `needle.json` beside this file: every query with the call it produced, its
confidence and its latency, so the threshold table in NEEDLE-BENCH.md can be
recomputed without a rerun.
"""
import json, pathlib, time
import needle

HERE = pathlib.Path(__file__).parent
DATA = json.loads((HERE / "cases.json").read_text())
GOALS = DATA["titles"]

# Described as well as the docs ask for: a sentence per tool saying WHEN to use
# it, a sentence per argument, and an enum on every one — which compiles into the
# decode grammar, so a title it has never seen cannot be emitted. A weaker
# declaration would be benchmarking the description, not the model.
TOOLS = [
    {
        "name": "filter_state",
        "description": "Show only the goals in one lifecycle state. Use when the person asks about a GROUP of goals by status, not about one named goal.",
        "parameters": {
            "type": "object",
            "properties": {
                "state": {
                    "type": "string",
                    "description": "queued = written but not started. running = a search is in flight. awaiting-human = a pull request is open and needs review. done = landed. failed = dead-lettered.",
                    "enum": ["queued", "running", "awaiting-human", "done", "failed"],
                }
            },
            "required": ["state"],
        },
    },
    {
        "name": "focus_goal",
        "description": "Select one goal on the graph and open its panel. Use when the person names a specific goal.",
        "parameters": {
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "The exact title of the goal.", "enum": GOALS}
            },
            "required": ["title"],
        },
    },
    {
        "name": "open_run",
        "description": "Open the run graph for one goal: what its branches did, what the gate said. Use when the person asks what HAPPENED, or asks for a run or a trace.",
        "parameters": {
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "The exact title of the goal whose run to open.", "enum": GOALS}
            },
            "required": ["title"],
        },
    },
]

KIND = {"filter_state": "state", "focus_goal": "focus", "open_run": "run"}


def main():
    agent = needle.Needle(tools=TOOLS)
    rows = []
    for case in DATA["cases"]:
        # Each query is its own turn. A session SHARES one conversation
        # (doc/apis.md: "later turns are bare queries against the same tools"), so
        # without this the 256-token window fills with earlier questions and every
        # answer is read in the context of the last one. Scored 5/25 before this
        # line and 11/25 after it — a harness bug worth more than any tuning.
        agent.reset()
        t = time.time()
        r = agent.complete(case["q"])
        ms = (time.time() - t) * 1000
        calls = r.get("function_calls") or []
        got = {"kind": "none"}
        if calls:
            c = calls[0]
            got = {"kind": KIND.get(c["name"], "none"), **c.get("arguments", {})}
        rows.append({**case, "got": got, "conf": r.get("confidence"),
                     "ms": round(ms, 1), "ram": r.get("peak_ram_mb")})

    (HERE / "needle.json").write_text(json.dumps(rows, indent=1))

    def hit(r):
        w, g = r["want"], r["got"]
        return w["kind"] == g["kind"] and w.get("state") == g.get("state") and w.get("title") == g.get("title")

    print(f"needle {sum(map(hit, rows))}/{len(rows)}")
    for r in rows:
        if not hit(r):
            print(f"  MISS {r['q']!r} -> {r['got']} want {r['want']} (conf {r['conf']})")

    print("\nthreshold  answered  correct-of-answered")
    for t in (0.0, 0.3, 0.5, 0.7, 0.9):
        a = [r for r in rows if (r["conf"] or 0) >= t]
        n = len([r for r in a if hit(r)])
        print(f"  {t:.1f}      {len(a):2d}/{len(rows)}     {n}/{len(a) or 1} = {100 * n / max(len(a), 1):3.0f}%")
    lat = sorted(r["ms"] for r in rows)
    print(f"\nlatency median {lat[len(lat)//2]:.0f}ms  max {lat[-1]:.0f}ms  "
          f"peak ram {max(r['ram'] or 0 for r in rows):.0f}MB")


if __name__ == "__main__":
    main()
