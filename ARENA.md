# arena — a multiplayer game (authoritative, rule-enforced interactive state)

A **backend class none of the other showcases cover**: two players share one
board and the *server* is the referee. `arena` is Connect Four — every move is
validated server-side (it's your turn, the column has room, the game is live),
win and draw are detected on the server, and the live board is streamed to both
players **and any number of spectators** over Server-Sent Events. The others are
request/response, streams, convergence, or background work; this is **interactive
authoritative state with enforced rules**.

Same shape as the rest: one **`arena-domain`** HTTP component that exports
`wasi:http` and imports only WIT contracts — **`records:store`** (one row per
game), **`id:generate`** (game ids + secret seat tokens), and the SSE loop from
[pulse](REALTIME.md). No bespoke crate beyond the Connect Four rules.

![Two players on one Connect Four board, side by side: Alice creates a game as Red, Bob joins as Yellow, and they alternate moves — each one validated server-side and streamed to both boards over SSE. Red stacks a column, the four-in-a-row lights up in both panes. A live recording of the running app.](docs/media/arena.gif)

## Why the server is the referee

A game is only fair if a client can't cheat, so all the rules live server-side:

| rule | enforced by |
|---|---|
| only a player may move | a secret **seat token** (from create/join) maps the caller to Red or Yellow; anyone else is `403` |
| only on your turn | the move's seat must equal the game's `turn`, else `403` |
| only into a column with room | the drop finds the lowest empty cell, else `409` "column full" |
| only while the game is live | moves after `finished` are `409` |
| **two moves can't both land** | the write uses the record store's **optimistic revision check** — a racing move gets `409` "board changed", so exactly one applies |

Win detection (four in a row along any of the four axes) and draw (full board)
run on the server too; the winning line comes back so every viewer highlights it.
The whole ruleset is exercised by `just e2e-arena`: out-of-turn, non-player,
illegal-column, and double-move all rejected; a scripted vertical win detected;
no moves after the end; and a **held-open SSE spectator seeing a move live**.

## How a game flows

1. `POST /api/games {name}` → creates a game (status `waiting`) and returns the
   creator's **Red** seat + secret token.
2. `POST /api/games/{id}/join {name}` → fills the **Yellow** seat, status →
   `active`, Red to move.
3. `POST /api/games/{id}/move {token, col}` → the server maps token → seat,
   checks turn + legality, drops the disc, detects win/draw, flips the turn, and
   stores it under the revision check.
4. `GET /api/games/{id}/events` holds a connection open and pushes the public
   board (tokens redacted) whenever the revision changes — players and spectators
   alike. Same in-guest streaming loop as pulse.

The board is a 42-char string (`row*7 + col`, row 0 = bottom); the public view
never leaks a seat token, so spectators can watch but not move.

## Run it

```bash
just host-arena     # native host + SPA on http://127.0.0.1:3039
# open two windows: "New game" in one, paste the id + "Join" in the other, play.
# open a third window on the same ?game= to spectate live.
just e2e-arena      # the rules + win-detection + live-SSE e2e
```

## Rungs left

- **Rematch + lobby polish** — a "play again" that resets the board; the lobby
  (`GET /api/games`) lists open games to click into.
- **More games behind the same shape** — tic-tac-toe, Othello: the create/join/
  move/SSE frame is game-agnostic; only the rules module changes.
- **Move clocks** — per-player timers via `sched:timer`, forfeiting on timeout.
