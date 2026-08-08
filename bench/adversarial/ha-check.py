"""Do two ingresses agree, and does killing one cost anything?

The ingress holds no state beyond a cache of inventory, so several should be able
to run. "Should" is the word this file exists to remove.
"""
import collections, json, sys, urllib.request

only = None
label = "ingress"
if "--only" in sys.argv:
    only = sys.argv[sys.argv.index("--only") + 1]
if "--label" in sys.argv:
    label = sys.argv[sys.argv.index("--label") + 1]


def burst(port, n, tag):
    seen, fail = collections.Counter(), 0
    for i in range(n):
        r = urllib.request.Request(
            f"http://127.0.0.1:{port}/api/ratelimit",
            data=json.dumps({"key": f"{tag}{i}"}).encode(),
            headers={"content-type": "application/json", "Host": "shop.eve.test"},
        )
        try:
            with urllib.request.urlopen(r, timeout=15) as resp:
                seen[resp.headers.get("x-comp-node", "?")] += 1
        except Exception:
            fail += 1
    return seen, fail


if only:
    seen, fail = burst(only, 30, "ha")
    print(f"    {label} served {sum(seen.values())}/30 over {len(seen)} node(s), {fail} failed")
else:
    a, fa = burst(8090, 30, "a")
    b, fb = burst(8095, 30, "b")
    print(f"    ingress A (:8090) -> {sum(a.values())}/30 over {len(a)} node(s), {fa} failed")
    print(f"    ingress B (:8095) -> {sum(b.values())}/30 over {len(b)} node(s), {fb} failed")
    print(f"    both see the same fleet: {sorted(a) == sorted(b)}")
