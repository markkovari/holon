#!/usr/bin/env bash
# Convert a Playwright .webm to an optimized, palette-based GIF.
# Usage: to-gif.sh <input.webm> <output.gif> [width] [fps] [start-seconds]
#
# `start-seconds` drops the head of the clip. Recording begins when the browser
# context does, so the first second is whatever the app shows before it has loaded
# — and that frame is the one a GIF is previewed by. Optional, default 0, so every
# existing caller is unaffected.
set -euo pipefail
IN="$1"; OUT="$2"; WIDTH="${3:-900}"; FPS="${4:-12}"; START="${5:-0}"
PAL="$(mktemp -t pal-XXXX).png"
trap 'rm -f "$PAL"' EXIT
ffmpeg -y -ss "$START" -i "$IN" -vf "fps=$FPS,scale=$WIDTH:-1:flags=lanczos,palettegen=stats_mode=diff" "$PAL" >/dev/null 2>&1
ffmpeg -y -ss "$START" -i "$IN" -i "$PAL" -lavfi "fps=$FPS,scale=$WIDTH:-1:flags=lanczos[x];[x][1:v]paletteuse=dither=bayer:bayer_scale=3" "$OUT" >/dev/null 2>&1
command -v gifsicle >/dev/null && gifsicle -O3 --lossy=60 "$OUT" -o "$OUT" >/dev/null 2>&1 || true
echo "$OUT ($(du -h "$OUT" | cut -f1))"
