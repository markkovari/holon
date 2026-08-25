#!/usr/bin/env python3
"""Print one line per wadm scaler, with the reason when it failed.

Reads `wadm.api.<lattice>.model.status.<app>` on stdin. Its own JSON is nested and
verbose enough that reading it raw is how a real failure gets missed — the petclinic
app on this cluster reports a 4 KB status message with the useful sentence in the
middle of it.
"""
import json
import sys

d = json.load(sys.stdin)
st = d.get("status", d)
scalers = st.get("scalers", [])
if not scalers:
    # wadm ignores a trait type it does not understand rather than refusing it, so
    # "no scalers" is what a v2 manifest on a v1 wadm looks like: deployed, running
    # nothing. Worth saying out loud.
    print("  no scalers — if this manifest was rendered --api v2, this wadm ignored it")
for s in scalers:
    print("  %-12s %-46s %s" % (s["kind"], s["name"][:46], s["status"]["type"]))
    if s["status"]["type"] == "failed":
        print("               %s" % s["status"]["message"][:160])
