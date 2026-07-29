# tempo — multi-person worktime logger (TEMPO.md)

Log time by project + category (or a pomodoro timer); see your contribution over
week/month/year/custom ranges, broken down by project and category — and as a
project **lead**, that project's whole distribution. Logging is gated by
**per-project membership**; owners edit/delete their own entries. See
[TEMPO.md](../../TEMPO.md).

A composed HTTP app on the native Rust host, with a **React + shadcn/ui** SPA.

```
ui/                      # Vite + React + TS + Tailwind + shadcn/ui + recharts source
public/ -> (built)       # `npm run build` emits ../dist, which the host serves
tests/tempo.rs           # e2e: auth + membership + edit/delete + range reports + timer
```

## Run

```bash
# from the repo root:
just host-tempo          # builds the UI (Vite) + composes tempo-domain + serves on :3040
```

Open `http://127.0.0.1:3040`: **register** (`admin` to create projects/categories
and assign membership; `member` to log). An admin adds you to a project as
**member** (log) or **lead** (log + see the project's team view). Log time or hit
**Start timer** for a pomodoro; the **Reports** tab has the charts and a **PDF**
button that downloads the current range as a PDF (rendered by the `pdf:codec`
component — no PDF library, no headless browser).

```bash
just e2e-tempo           # the auth + membership + aggregation + timer e2e (spawns the host)
# work on the UI live:
cd examples/tempo/ui && npm install && npm run dev   # (proxy /api to :3040)
```

## Deploy (self-contained, cheapest)

No wasmCloud needed — `comp-host` runs the component + serves the SPA + provides
Redis-backed storage in one process. `Dockerfile` builds that as one image;
`docker-compose.yml` runs it pointed at any Redis via **`REDIS_URL`** — HTTP
only, a POC that fits a $6 droplet:

```bash
just compose-tempo && just build-tempo-ui            # produce the .wasm + dist
cp .env.example .env                                 # set REDIS_URL
# hosted Redis (Upstash / DO — TLS): REDIS_URL=rediss://default:PW@host:6379
docker compose --env-file .env up -d --build         # -> http://localhost:8080
# or a bundled local Valkey instead of a hosted one:
docker compose --env-file .env --profile local-redis up -d --build   # REDIS_URL=redis://redis:6379
```

For HTTPS later, put a DigitalOcean Load Balancer or Cloudflare in front of
`:8080` — no change to the compose. See [TEMPO.md](../../TEMPO.md) for the droplet
walkthrough and the wasmCloud/GHCR path for scale.
