# The showcases

One file per application, each one a real thing that runs: composed from
capability components, served by `comp-host`, and driven end to end by a `just
e2e-*` recipe rather than by a screenshot.

They live here rather than at the repository root, where thirty-eight markdown
files made it impossible to see that `README.md` and `ROADMAP.md` were the two
that mattered. Nothing else changed — every one of them still describes what it
did before.

To run one, find its recipe: `just --list | grep e2e-`. To see what a showcase is
built out of, ask the component rather than this table — `just plug-wiring
<component>` derives it from the artifact's own imports.

| doc | app | what it is |
| --- | --- | --- |
| [ARENA](ARENA.md) | `arena` | a multiplayer game (authoritative, rule-enforced interactive state) |
| [AUTHGATE](AUTHGATE.md) | `authgate` | TOTP two-factor enrollment + challenge-response login |
| [BOOKED](BOOKED.md) | `booked` | a Calendly-lite booking service (no double-books) |
| [BOOKS](BOOKS.md) | `books` | double-entry bookkeeping (the books always balance) |
| [BUZZ](BUZZ.md) | `buzz` | a live multiplayer quiz game (Kahoot-style) |
| [CONDUIT](CONDUIT.md) | `conduit` | the RealWorld spec, composed from capability contracts |
| [CONSOLE](CONSOLE.md) | `console` | the Holon console — author a goal as a PR, read a run as a graph |
| [DASHBOARDS](DASHBOARDS.md) | `dashboards` | metric panels, charts rendered on the server |
| [DROP](DROP.md) | `drop` | a presigned direct-upload drop-box |
| [ESHOP](ESHOP.md) | `eshop` | eShopOnDapr recreated on wasmCloud |
| [EXPERIMENT](EXPERIMENT.md) | `experiment` | context-based A/B testing, from assignment to conversion |
| [FLAGS](FLAGS.md) | `flags` | a live feature-rollout console (set a rule, watch it propagate) |
| [GATE](GATE.md) | `gate` | a durable traffic-shaping gateway (the Golem worker patterns) |
| [HELPDESK](HELPDESK.md) | `helpdesk` | a mid-sized SaaS over composed capability contracts |
| [JOBS](JOBS.md) | `jobs` | a durable background-job queue (with a swappable execution backend) |
| [LMS](LMS.md) | `lms` | a learning platform (courses, auto-graded quizzes, gradebook, certificates) |
| [MESH](MESH.md) | `mesh` | resilient upstream calls (the breaker trips, the app stays up) |
| [PASSKEY](PASSKEY.md) | `passkey` | passwordless sign-in (the phishing-resistant one) |
| [PASTE](PASTE.md) | `bin` | a paste / gist bin over a pure-compute pipeline |
| [PAYEES](PAYEES.md) | `payees` | a payee book with IBAN-validated bank details |
| [PIPELINE](PIPELINE.md) | `pipeline` | a reliable event pipeline (outbox → dispatch → DLQ → replay) |
| [RATELIMIT](RATELIMIT.md) | `ratelimit` | a live throttle wall (lockout + quota, watched) |
| [REALTIME](REALTIME.md) | `pulse` | a realtime chat room, composed from capability contracts |
| [REPORT](REPORT.md) | `report` | batch CSV import → typed validate → paged report → CSV export |
| [SAGA](SAGA.md) | `saga` | a durable trip-booking saga, composed from capability contracts |
| [SCRIBE](SCRIBE.md) | `scribe` | a collaborative document editor (convergence, made live) |
| [SEARCH](SEARCH.md) | `search` | faceted search-as-you-type over a real corpus |
| [STASH](STASH.md) | `stash` | a note stash you export as a .zip |
| [STATUS](STATUS.md) | `status` | a status page / uptime monitor |
| [STUDIO](STUDIO.md) | `studio` | components describe themselves, and compose themselves |
| [TEMPO](TEMPO.md) | `tempo` | a multi-person worktime logger (with charts) |
| [TRACK](TRACK.md) | `track` | a Linear-lite project tracker (the complex composition) |
| [TRANSIT](TRANSIT.md) | `transit` | public-transport ticketing (buy a QR, validate with a camera) |

## Capability notes

Not applications: these describe a capability or a provider that the showcases
build on.

| doc | subject | what it is |
| --- | --- | --- |
| [CRDT](../capabilities/CRDT.md) | `crdt` | conflict-free convergence (the primitive `scribe` builds on) |
| [GOLEM](../capabilities/GOLEM.md) | `golem-provider` | a native wRPC→Golem durable-worker capability provider |
| [USAGE](../capabilities/USAGE.md) | `Using auth:identity` |  |

## What is deliberately not here

`docs/adr/` holds the decisions and supersedes them in place; `docs/CURRENT.md`
is the state of the loop; `docs/PLATFORM.md` is the platform plan. A showcase doc
describes one app and does not try to be any of those.

