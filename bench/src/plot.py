#!/usr/bin/env python3
"""Render benchmark charts from results-inproc.json + results-http.json."""
import json, pathlib
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

here = pathlib.Path(__file__).resolve().parent.parent
inproc = json.loads((here / "results-inproc.json").read_text())["results"]
http = json.loads((here / "results-http.json").read_text())["results"]

def by_op(rs): return {r["op"]: r for r in rs}
ip, hp = by_op(inproc), by_op(http)

# Horizontal bars: ~50 ops don't fit legible vertical labels.
ops = [r["op"] for r in inproc]
means = [r["meanNs"] / 1e3 for r in inproc]  # µs

def family(op):
    return op.split(".")[0].split("(")[0].split(" ")[0]

fams = [family(o) for o in ops]
uniq = list(dict.fromkeys(fams))
cmap = plt.get_cmap("tab20")
fam_color = {f: cmap(i % 20) for i, f in enumerate(uniq)}
colors = [fam_color[f] for f in fams]

n = len(ops)
fig, ax = plt.subplots(figsize=(10, max(8, n * 0.26)))
y = range(n)
bars = ax.barh(list(y), means, color=colors)
ax.set_xscale("log")
ax.set_yticks(list(y))
ax.set_yticklabels(ops, fontsize=8)
ax.invert_yaxis()  # first op at top
ax.set_xlabel("mean latency (µs, log scale)")
ax.set_title("In-process op latency by capability (jco, single Node process)")
ax.grid(axis="x", which="both", linestyle=":", alpha=0.4)
xmax = max(means)
for b, m in zip(bars, means):
    ax.text(b.get_width() * 1.05, b.get_y() + b.get_height() / 2,
            f"{m:,.1f}", ha="left", va="center", fontsize=7)
ax.set_xlim(right=xmax * 2.2)  # headroom for the value labels
handles = [plt.Rectangle((0, 0), 1, 1, color=fam_color[f]) for f in uniq]
ax.legend(handles, uniq, fontsize=7, ncol=1, loc="upper left",
          bbox_to_anchor=(1.01, 1.0), framealpha=0.9, title="capability")
fig.tight_layout(); fig.savefig(here / "bench-inproc.png", dpi=130); plt.close(fig)

fig, ax = plt.subplots(figsize=(8, 4.5))
hops = [r["op"] for r in http]
hmean = [r["meanNs"] / 1e6 for r in http]
hp99 = [r["p99Ns"] / 1e6 for r in http]
x = range(len(hops))
ax.bar([i - 0.2 for i in x], hmean, width=0.4, label="mean", color="#54a24b")
ax.bar([i + 0.2 for i in x], hp99, width=0.4, label="p99", color="#e45756")
ax.set_xticks(list(x)); ax.set_xticklabels(hops, rotation=20)
ax.set_ylabel("latency (ms)")
ax.set_title("HTTP roundtrip latency (wasmCloud k8s, in-cluster client)")
ax.legend()
fig.tight_layout(); fig.savefig(here / "bench-http.png", dpi=130); plt.close(fig)

pairs = [
    ("introspect", "GET /me", "verify token"),
    ("authorize", "POST /verify", "authorize+perm"),
    ("login", "POST /login", "login (argon2)"),
    ("register", "POST /register", "register (argon2)"),
]
labels, ip_us, hp_us = [], [], []
for ipk, hpk, lbl in pairs:
    if ipk in ip and hpk in hp:
        labels.append(lbl)
        ip_us.append(ip[ipk]["meanNs"] / 1e3)
        hp_us.append(hp[hpk]["meanNs"] / 1e3)
fig, ax = plt.subplots(figsize=(8.5, 4.5))
x = range(len(labels))
ax.bar([i - 0.2 for i in x], ip_us, width=0.4, label="in-process", color="#4c78a8")
ax.bar([i + 0.2 for i in x], hp_us, width=0.4, label="HTTP roundtrip", color="#f58518")
ax.set_yscale("log")
ax.set_xticks(list(x)); ax.set_xticklabels(labels)
ax.set_ylabel("mean latency (µs, log)")
ax.set_title("In-process vs HTTP roundtrip (same op) — transport + host overhead")
ax.legend()
for i, (a, b) in enumerate(zip(ip_us, hp_us)):
    ax.text(i + 0.2, b, f"{b/a:,.0f}×", ha="center", va="bottom", fontsize=8)
fig.tight_layout(); fig.savefig(here / "bench-overhead.png", dpi=130); plt.close(fig)

print("wrote bench-inproc.png, bench-http.png, bench-overhead.png")
