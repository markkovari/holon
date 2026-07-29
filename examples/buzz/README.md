# buzz — a live multiplayer quiz game (BUZZ.md)

Kahoot-style: a **host** signs in and runs a game (gets a PIN); **players** join
anonymously with the PIN + a nickname on their own devices and buzz in during
each question. On reveal the app grades **speed-weighted** (faster correct = more
points) and updates a live leaderboard. Real-time is client polling — comp-host is
request/response. See [BUZZ.md](../../BUZZ.md).

A composed HTTP app on the native Rust host, with a **React + shadcn/ui** SPA
(a host big-screen + a player controller).

```
ui/                      # Vite + React + TS + Tailwind + shadcn/ui source
public/ -> (built)       # `npm run build` emits ../dist, which the host serves
tests/buzz.rs            # e2e: game loop + speed-weighted scoring + leaderboard + podium
```

## Run

```bash
# from the repo root:
just host-buzz           # composes the component + builds the UI + serves on :3049
```

Open `http://127.0.0.1:3049` on one device and choose **Host a game** (sign in —
you get a demo quiz and a PIN). Open it on other devices, enter the **PIN** + a
nickname, and play; the host clicks **Start / Reveal / Next**.

```bash
just e2e-buzz            # the game loop + speed-weighted scoring
# work on the UI live:
cd examples/buzz/ui && npm install && npm run dev
```
