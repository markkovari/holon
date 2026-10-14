#!/usr/bin/env python3
"""Food photo connector: a photo of a meal in, an estimate of what is on the plate out.

  foodphoto.py --key-file ~/.comp-secrets/deepseek list      stdin {}
  foodphoto.py --key-file ~/.comp-secrets/deepseek analyze   stdin {"path": "lunch.jpg", "note": "optional"}

Photos live in the calling agent's own inbox: `$HOLON_WORKSPACE/inbox`, where the chat bridge
puts a photo you send (`--inbox` overrides it; with neither it is ~/.holon-agents/inbox/food).
`path` is the file name in that folder and cannot leave it. `list` names the newest photos first, `analyze`
sends one to a vision model (DeepSeek, `deepseek-chat`) and prints the items it sees with
grams, calories and macros, a confidence, and what it is unsure of. Only api.deepseek.com is
contacted. An estimate from a photo is a guess about portions, and the output says so.
"""
import argparse
import base64
import json
import mimetypes
import os
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request

API = "https://api.deepseek.com/chat/completions"
EXTS = {".jpg", ".jpeg", ".png", ".webp", ".gif", ".heic"}
MAX_BYTES = 8 * 1024 * 1024
SHRINK_ABOVE = 1_500_000  # bytes; a phone photo is 3-5 MB and the model does not need it

PROMPT = """You are estimating the nutrition of a meal from a photo, for an athlete who tracks food.
Reply with ONE JSON object and nothing else:
{"items": [{"name": str, "grams": int, "kcal": int, "protein_g": int, "carbs_g": int, "fat_g": int}],
 "confidence": "low" | "medium" | "high",
 "unsure": [str],
 "notes": str}
Rules: list each distinct food you can see, with a portion in COOKED weight judged from the plate
(a dinner plate is about 26 cm), including visible cooking fat, skin and sauces. Do not invent items
you cannot see. If something is hidden or ambiguous, say so in "unsure" and lower the confidence.
Use plain numbers, no ranges. Never claim precision: a photo only supports a rough estimate."""


def fail(message):
    print(message, file=sys.stderr)
    sys.exit(1)


def inbox_path(inbox, rel):
    root = os.path.realpath(os.path.expanduser(inbox))
    path = os.path.realpath(os.path.join(root, rel))
    if os.path.commonpath([root, path]) != root:
        fail("that path is outside the photo inbox")
    if os.path.splitext(path)[1].lower() not in EXTS:
        fail("not a photo (jpg, png, webp or gif)")
    if not os.path.isfile(path):
        fail(f"no such photo in the inbox: {rel}. Use `list` to see what is there.")
    return path


def list_photos(inbox):
    root = os.path.realpath(os.path.expanduser(inbox))
    if not os.path.isdir(root):
        print("the photo inbox is empty (it does not exist yet)")
        return
    files = [e for e in os.scandir(root) if e.is_file() and os.path.splitext(e.name)[1].lower() in EXTS]
    files.sort(key=lambda e: e.stat().st_mtime, reverse=True)
    if not files:
        print("the photo inbox is empty")
    import datetime
    for e in files[:20]:
        when = datetime.datetime.fromtimestamp(e.stat().st_mtime).strftime("%Y-%m-%d %H:%M")
        print(f"{when} | {e.name} | {e.stat().st_size // 1024} KB")


def load_image(path):
    """The photo's bytes, shrunk with the system `sips` when it is large. An iPhone's HEIC is
    converted to JPEG, which the vision model takes."""
    size = os.path.getsize(path)
    if size > MAX_BYTES * 4:
        fail("photo is too large")
    heic = os.path.splitext(path)[1].lower() == ".heic"
    if size > SHRINK_ABOVE or heic:
        with tempfile.TemporaryDirectory() as tmp:
            out = os.path.join(tmp, "small.jpg")
            try:
                subprocess.run(["/usr/bin/sips", "-Z", "1280", "-s", "format", "jpeg", path, "--out", out],
                               check=True, capture_output=True, timeout=30)
                with open(out, "rb") as f:
                    return f.read(), "image/jpeg"
            except (OSError, subprocess.SubprocessError):
                if heic:
                    fail("could not convert the HEIC photo (needs the macOS `sips`)")
                # no sips (not macOS): send it as it is
    if size > MAX_BYTES:
        fail("photo is too large")
    with open(path, "rb") as f:
        return f.read(), mimetypes.guess_type(path)[0] or "image/jpeg"


def ask(key, image, mime, note, model):
    text = PROMPT + (f"\nThe athlete says: {note}" if note else "")
    body = {
        "model": model,
        "temperature": 0,
        "max_tokens": 900,
        "response_format": {"type": "json_object"},
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": text},
            {"type": "image_url", "image_url": {"url": f"data:{mime};base64,{base64.b64encode(image).decode()}"}},
        ]}],
    }
    req = urllib.request.Request(API, data=json.dumps(body).encode(), headers={
        "Authorization": f"Bearer {key}", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=90) as r:  # nosemgrep: python.lang.security.audit.dynamic-urllib-use-detected.dynamic-urllib-use-detected
            return json.load(r)["choices"][0]["message"]["content"]
    except urllib.error.HTTPError as e:
        fail(f"the vision model refused: HTTP {e.code} {e.read().decode('utf-8', 'replace')[:200]}")
    except (urllib.error.URLError, TimeoutError) as e:
        fail(f"could not reach the vision model: {e}")


def render(name, raw):
    try:
        d = json.loads(raw)
        items = d["items"]
    except (ValueError, KeyError, TypeError):
        fail(f"the vision model did not return the estimate as JSON: {raw[:200]}")
    num = lambda it, k: int(round(float(it.get(k) or 0)))
    lines = [f"photo: {name}  (confidence: {d.get('confidence', 'unknown')})"]
    tot = {"kcal": 0, "protein_g": 0, "carbs_g": 0, "fat_g": 0}
    for it in items:
        for k in tot:
            tot[k] += num(it, k)
        lines.append(f"- {it.get('name', '?')}: ~{num(it, 'grams')} g | {num(it, 'kcal')} kcal | "
                     f"P {num(it, 'protein_g')} | C {num(it, 'carbs_g')} | F {num(it, 'fat_g')}")
    lines.append(f"total: ~{tot['kcal']} kcal | P {tot['protein_g']} g | C {tot['carbs_g']} g | F {tot['fat_g']} g")
    if d.get("unsure"):
        lines.append("unsure: " + "; ".join(str(u) for u in d["unsure"]))
    if d.get("notes"):
        lines.append("note: " + str(d["notes"]))
    lines.append("(an estimate from a photo: portions are guesses. Ask for grams if it matters.)")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--key-file", required=True)
    workspace = os.environ.get("HOLON_WORKSPACE")
    ap.add_argument("--inbox", default=os.path.join(workspace, "inbox") if workspace else "~/.holon-agents/inbox/food")
    ap.add_argument("--model", default="deepseek-chat")
    ap.add_argument("cmd", choices=["list", "analyze"])
    args = ap.parse_args()
    try:
        given = json.loads(sys.stdin.read() or "{}")
    except ValueError:
        fail("stdin must be JSON")
    if args.cmd == "list":
        return list_photos(args.inbox)
    rel = str(given.get("path", "")).strip()
    if not rel:
        fail('analyze needs {"path": "<photo in the inbox>"}')
    path = inbox_path(args.inbox, rel)
    with open(os.path.expanduser(args.key_file)) as f:
        key = f.read().strip()
    image, mime = load_image(path)
    print(render(os.path.basename(path), ask(key, image, mime, str(given.get("note", ""))[:300], args.model)))


if __name__ == "__main__":
    main()
