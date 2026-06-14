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

# ---- 1. in-process op latency (mean µs, log scale) ----
fig, ax = plt.subplots(figsize=(9, 4.5))
ops = [r["op"] for r in inproc]
means = [r["meanNs"] / 1e3 for r in inproc]  # µs
bars = ax.bar(ops, means, color="#4c78a8")
ax.set_yscale("log")
ax.set_ylabel("mean latency (µs, log)")
ax.set_title("In-process op latency (jco, single Node process)")
ax.tick_params(axis="x", rotation=30)
for b, m in zip(bars, means):
    ax.text(b.get_x() + b.get_width() / 2, m, f"{m:,.1f}µs", ha="center", va="bottom", fontsize=8)
fig.tight_layout(); fig.savefig(here / "bench-inproc.png", dpi=130); plt.close(fig)

# ---- 2. HTTP roundtrip latency (mean + p99, ms) ----
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

# ---- 3. in-process vs HTTP overhead, shared ops (log µs) ----
# map comparable ops
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
