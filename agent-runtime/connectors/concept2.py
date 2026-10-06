#!/usr/bin/env python3
"""Concept2 logbook connector: read a PUBLIC profile's workouts, one at a time. No API key.

  concept2.py --profile 2586473 list      stdin {"limit": 10, "page": 1}
  concept2.py --profile 2586473 workout   stdin {"id": "121366059"}

`list` prints the newest workouts first, one line each, with the id `workout` takes.
`workout` prints one session's totals (time, distance, pace, heart rate, watts, stroke rate,
drag factor) and its splits. Output is short on purpose: a small model reads it whole.
Only log.concept2.com is contacted, and only pages the profile makes public.
"""
import argparse
import html
import json
import re
import sys
import urllib.request

BASE = "https://log.concept2.com"
UA = "Mozilla/5.0 (holon concept2 connector)"


def fetch(path):
    req = urllib.request.Request(BASE + path, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=20) as r:
        return r.read().decode("utf-8", "replace")


def text(h):
    h = re.sub(r"<script.*?</script>|<style.*?</style>", " ", h, flags=re.S)
    return re.sub(r"\s+", " ", html.unescape(re.sub(r"<[^>]+>", " ", h))).strip()


def listing(profile, limit, page):
    h = fetch(f"/profile/{profile}/log" + (f"?page={page}" if page > 1 else ""))
    rows = re.findall(r"<tr[ >].*?</tr>", h, flags=re.S)
    out = []
    for r in rows:
        cells = [text(c) for c in re.findall(r"<td[^>]*>(.*?)</td>", r, flags=re.S)]
        link = re.search(r"/log/(\d+)", r)
        if len(cells) >= 5 and link and re.fullmatch(r"\d\d/\d\d/\d\d", cells[0]):
            m, d, y = cells[0].split("/")
            note = f" | {cells[5]}" if len(cells) > 5 and cells[5] else ""
            out.append(f"20{y}-{m}-{d} | {cells[1]} | {cells[2]} | {cells[3]}/500m | {cells[4]} | id {link.group(1)}{note}")
    if not out:
        return "no workouts found on that page"
    return "\n".join(out[:limit])


def num(pattern, t):
    m = re.search(pattern, t)
    return m.group(1) if m else None


def workout(profile, wid):
    if not re.fullmatch(r"\d{4,12}", wid):
        sys.exit("`id` must be the digits from `list`")
    t = text(fetch(f"/profile/{profile}/log/{wid}"))
    title = num(r"^(.*?) \| Concept2 Logbook", t) or "workout"
    date = num(r"(\w+ \d\d, \d{4}) \d\d:\d\d:\d\d Workout", t)
    fields = [
        ("distance", num(r"([\d,]+) Meters", t), "m"),
        ("time", num(r"([\d:.]+) Time", t), ""),
        ("pace", num(r"([\d:.]+) Pace", t), "/500m"),
        ("heart rate avg", num(r"(\d+) Heart Rate", t), " bpm"),
        ("avg watts", num(r"Watts (\d+)", t), " W"),
        ("stroke rate", num(r"Stroke Rate (\d+)", t), " spm"),
        ("stroke count", num(r"Stroke Count (\d+)", t), ""),
        ("drag factor", num(r"Drag Factor (\d+)", t), ""),
        ("calories", num(r"(\d+) Calories ", t), ""),
    ]
    lines = [f"{title} ({date or '?'}, id {wid})"]
    lines.append("; ".join(f"{k} {v}{u}" for k, v, u in fields if v))
    sp = re.search(r"Splits Time Meters Pace Watts Cal/Hr Cal S/M(?: HR)? (.*?) Click and drag", t)
    if sp:
        nums = re.findall(r"[\d:.,]+", sp.group(1))
        rows = [nums[i:i + 8] for i in range(0, len(nums) - 7, 8)] or [nums[i:i + 7] for i in range(0, len(nums) - 6, 7)]
        if len(rows) > 1:
            lines.append("splits (time | metres | pace | watts | spm):")
            for r in rows[1:]:
                lines.append(f"  {r[0]} | {r[1]}m | {r[2]} | {r[3]}W | {r[6]}")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--profile", required=True)
    ap.add_argument("op", choices=["list", "workout"])
    a = ap.parse_args()
    if not re.fullmatch(r"\d{1,12}", a.profile):
        sys.exit("--profile must be digits")
    try:
        args = json.loads(sys.stdin.read() or "{}")
    except ValueError:
        args = {}
    try:
        if a.op == "list":
            print(listing(a.profile, max(1, min(int(args.get("limit", 10)), 30)), max(1, int(args.get("page", 1)))))
        else:
            print(workout(a.profile, str(args.get("id", ""))))
    except Exception as e:  # network or markup change: say so, never guess
        sys.exit(f"could not read the logbook: {e}")


if __name__ == "__main__":
    main()
