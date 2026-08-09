"""One line per oha run, from its JSON. Errors are printed even when zero, because
a run that reports only percentiles reads as clean whether or not it was."""
import json, sys

path, label = sys.argv[1], sys.argv[2]
try:
    d = json.load(open(path))
except Exception as e:
    print(f"  {label:9} no result ({e}); see {path}.err")
    sys.exit(0)
s = d.get("summary", {})
p = d.get("latencyPercentiles", {})
codes = d.get("statusCodeDistribution", {})
errs = d.get("errorDistribution", {})
ok = sum(v for k, v in codes.items() if k.startswith("2"))
non2xx = sum(v for k, v in codes.items() if not k.startswith("2"))
# oha counts requests still in flight when the clock runs out as errors. With
# -c 200 that is 200 of them every run, and calling that "failed" overstates every
# result by exactly the connection count. Split it out.
aborted = sum(v for k, v in errs.items() if "deadline" in k)
transport = sum(errs.values()) - aborted
print(f"  {label:9} {s.get('requestsPerSec', 0):8.0f} rps   "
      f"p50 {1000 * p.get('p50', 0):7.1f} ms   p99 {1000 * p.get('p99', 0):8.1f} ms   "
      f"max {1000 * s.get('slowest', 0):8.1f} ms   "
      f"{ok} ok / {non2xx} non-2xx / {transport} failed / {aborted} in flight at end")
real = {k: v for k, v in errs.items() if "deadline" not in k}
if real:
    for k, v in sorted(real.items(), key=lambda kv: -kv[1])[:3]:
        print(f"  {'':9} {v:>8} x {k[:80]}")
if non2xx:
    print(f"  {'':9} codes: {dict(codes)}")
