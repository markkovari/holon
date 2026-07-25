# buzz — a live multiplayer quiz game (Kahoot-style)

A real-time party quiz. A **host** signs in, picks a quiz, and starts a game —
the app mints a short **PIN**. **Players** join anonymously with that PIN and a
nickname on their own devices; the host drives the game through its phases
(**lobby → question → reveal → … → final**), everyone buzzes in during each
question, and on reveal the app grades each answer **speed-weighted** (a correct
answer earns more the sooner it lands) and updates a **live leaderboard**.

Same shape as the other showcases: one **`buzz-domain`** HTTP component that
exports `wasi:http` and imports only WIT contracts — the composed **auth-guard**
(`auth:identity`) for the host, **`records:store`** for the game state, plus
`wasi:random` (the PIN) and the wall clock (timing). No bespoke auth or storage.
The frontend is a **React + shadcn/ui** SPA with two faces: a **host big-screen**
and a **player controller**.

![The buzz game on two screens: the host big-screen shows a giant game PIN and the roster filling as players join, then a question with four colored/shaped options (red ▲, blue ◆, yellow ●, green ■), a live “answered” count, and after Reveal the correct option ringed with a ✓ plus a leaderboard; a player's phone shows the four color buttons, a “Locked in” state after tapping, and a big green “Correct! +970” on reveal. A live recording of the running React app.](docs/media/buzz.gif)

## Real-time without a socket

vet-host is request/response, so real-time is **polling**: the host screen and
each player's controller `GET` their view a few times a second, and the host's
`start` / `reveal` / `next` `POST`s move the shared game (in `records:store`)
between phases. At Kahoot's ~second cadence that's indistinguishable from push —
and every device converges on the same server state. (The repo's `arena` shows
the SSE variant of the same idea.)

## The scoring (why speed matters)

On **reveal**, each answer for the current question is graded: wrong scores
**0**; a correct answer scores `round(1000 · (1 − elapsed/limit · ½))`, where
`elapsed` is how long after the question opened the answer arrived — so an
**instant** correct answer is the full **1000**, one at the **buzzer** is **500**,
and a **wrong** one is **0**. The e2e pins exactly this: three players answer, and
a faster-correct beats a slower-correct beats a wrong (`0`), with the leaderboard
ranked accordingly.

## The data model

- **quizzes** — `{host, title, questions:[{prompt, options[], answer, time_limit}]}`.
  A fresh host account is seeded a demo quiz ("WIT Warm-up").
- **games** — `{pin, quiz, host, phase, current, q_started_ms}`. The PIN is a
  6-digit code minted with `wasi:random`.
- **players** — `{game(pin), nickname, score}` (anonymous; the join returns an id
  the device keeps).
- **answers** — `{game, q, player, option, at_ms, correct, points}`, graded on reveal.

Gates: the answer key is never sent to players; you can only **join in the
lobby**, only **answer during a question**, only **once per question**, and only
the **host** (authenticated, and the game's owner) can drive the phases.

## Run it

```bash
just host-buzz    # composes the component, builds the React UI, serves on :3049
# open on one device and sign in to HOST (you get a demo quiz + a PIN);
# open on other devices and JOIN with the PIN + a nickname.
just e2e-buzz     # the game loop + speed-weighted scoring + leaderboard + podium
```

The frontend lives in `examples/buzz/ui` (Vite + React + shadcn/ui); the host
screen and player controller both poll their view.

## Rungs left

- **Server-side timer** — auto-reveal when the clock runs out (a `sched:timer`)
  instead of the host clicking Reveal.
- **Push instead of poll** — the `arena` SSE loop for lower latency.
- **Streaks + bonus** — extra points for consecutive correct answers.
- **QR to join** — a `qr:encode` of the join URL on the lobby screen.
