# pulse — a realtime chat room on the native Rust host

A live chat room as ONE composed wasm HTTP component (`pulse-domain` +
record-store + event-bus + id-generate), served by the native Rust host. Post a
message and it streams to every open window over a held-open **Server-Sent
Events** connection — real server push on wasip2, no WebSocket. See
[`../../REALTIME.md`](../../REALTIME.md).

The axis no other showcase covers: a **sustained connection**, not
request/response. The host spawns the guest as a task and streams the response
body while the guest keeps writing `data:` frames.

![two panes chatting live over SSE](../../docs/media/pulse.gif)

## Try it

```bash
just host-pulse      # serve the SPA + API on :3015
# open http://127.0.0.1:3015 in TWO windows (or ?name=Ada / ?name=Bob) and chat
just e2e-pulse       # Rust e2e: a held-open SSE reader gets a message posted by another request
```

```bash
# tail a room from the terminal:
curl -sN localhost:3015/api/rooms/lobby/events &
curl -s localhost:3015/api/rooms/lobby/messages -d '{"user":"ada","text":"hi"}'
```

## What runs where

| Layer | Language |
|---|---|
| `pulse-domain` (routing + the SSE stream loop) | Rust → wasm |
| record-store, event-bus, id-generate | Rust → wasm |
| host serving the composed `.wasm` + SPA | Rust (`host/`, wasmtime) |
| browser client | native `EventSource` (SSE) |
| e2e (`ureq`, streaming reader) | Rust |
