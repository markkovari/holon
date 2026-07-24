# arena — multiplayer Connect Four (ARENA.md)

Two players share one board; the server is the referee — every move validated,
win/draw detected, the live board streamed to both players and spectators over
SSE. See [ARENA.md](../../ARENA.md) for the write-up.

A composed HTTP app on the native Rust host, so this directory holds the board
SPA + a Rust e2e (not a jco harness).

```
public/index.html        # the Connect Four board (create / join / play, live over SSE)
tests/arena.rs           # e2e: create/join, rule enforcement, a win, live spectator
```

## Run

```bash
# from the repo root:
just host-arena          # composes arena-domain (+ records + ids); SPA on :3039
```

Open two windows on `http://127.0.0.1:3039`: **New game** in one, paste the game
id + **Join** in the other, and play. Open a third window on the same `?game=<id>`
to spectate the live board.

```bash
just e2e-arena           # the rules + win + live-SSE e2e (spawns the host)
```
