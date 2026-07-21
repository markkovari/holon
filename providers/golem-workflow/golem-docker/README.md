# golem-docker — Golem via docker-compose (production-shaped infra)

Golem's **official** `published-postgres` stack, vendored from
`golemcloud/golem` `docker-examples/published-postgres`. Nine services: an nginx
router (:9881), postgres, redis, and the golem registry / worker / shard-manager
/ component-compilation / worker-executor / debugging services.

```bash
docker compose up -d          # brings the whole stack up
docker compose down -v        # tear down + wipe volumes
```

## Status — read this before using it for the e2e

The stack **stands up cleanly here** (all 9 services run). But driving the
provider e2e *through docker* additionally needs the CLI authenticated against
the **production registry-service**:

```bash
golem profile new docker --url http://localhost:9881 \
  --static-token "$ADMIN_TOKEN" --set-active     # ADMIN_TOKEN from .env
```

In this environment that returned `AUTH_UNAUTHORIZED: Token not found` — the
example `ADMIN_TOKEN` was not seeded into the registry, most likely a **version
skew**: the golem CLI/binary here is `v1.5.5` while the compose pins images at
`GOLEM_IMAGES_VERSION=v1.5.0`, and the CLI's account/API paths didn't match the
older server. `registry-service:v1.5.5` does exist on Docker Hub — bumping the
`.env` to a fully version-matched image set is the likely fix, but wasn't
confirmed here.

**So:** the *verified* live e2e (`just golem-e2e`, `../e2e.sh`) uses Golem's
**single-binary dev server** (`golem server run`) — zero-auth, one process,
confirmed working end to end. This compose is the reproducible, production-shaped
alternative once the CLI↔server versions are matched and a token is seeded — the
extra ops the dev binary sidesteps. That trade (one binary + no auth vs. a
7-service stack + token auth) is exactly why the dev binary is the default here.
