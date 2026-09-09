# The showcases

One file per application, each one a real thing that runs: composed from
capability components, served by `comp-host`, and driven end to end by a
`cargo test` against `examples/<app>` rather than by a screenshot.

They live here rather than at the repository root, where thirty-eight markdown
files made it impossible to see that `README.md` and `ROADMAP.md` were the two
that mattered. Nothing else changed — every one of them still describes what it
did before.

To run one: `cargo xtask compose <app> && cargo test --manifest-path
examples/<app>/Cargo.toml`. `cargo xtask list` names every registered app. To
see what a showcase is built out of, ask the component rather than this table —
`comp-plug <component> --wiring` derives it from the artifact's own imports.

| doc | app | what it is |
| --- | --- | --- |
| [ACADEMIC-REVIEW](ACADEMIC-REVIEW.md) | `academic-review` | a peer-review submission tracker |
| [ARENA](ARENA.md) | `arena` | a multiplayer game (authoritative, rule-enforced interactive state) |
| [AUTHGATE](AUTHGATE.md) | `authgate` | TOTP two-factor enrollment + challenge-response login |
| [BINDER](BINDER.md) | `binder` | a Pokémon card collection: photograph a card, correct what the AI got wrong, and watch what it is worth |
| [BOOKED](BOOKED.md) | `booked` | a Calendly-lite booking service (no double-books) |
| [BOOKS](BOOKS.md) | `books` | double-entry bookkeeping (the books always balance) |
| [BUZZ](BUZZ.md) | `buzz` | a live multiplayer quiz game (Kahoot-style) |
| [CLIPBOARD-SYNC](CLIPBOARD-SYNC.md) | `clipboard-sync` | cross-device clipboard sync, fronting `desktop-clipboard` (ADR-0095) |
| [CONDUIT](CONDUIT.md) | `conduit` | the RealWorld spec, composed from capability contracts |
| [CONSOLE](CONSOLE.md) | `console` | the Holon console — author a goal as a PR, read a run as a graph |
| [CRON-SCHEDULER](CRON-SCHEDULER.md) | `cron-scheduler` | a cron-expression job scheduler, fronting `system-cron` (ADR-0095) |
| [DASHBOARDS](DASHBOARDS.md) | `dashboards` | metric panels, charts rendered on the server |
| [DESKTOP-NOTIFIER](DESKTOP-NOTIFIER.md) | `desktop-notifier` | a notification center, fronting `ui-notifier` (ADR-0095) |
| [DEVICE-RADAR](DEVICE-RADAR.md) | `device-radar` | an IoT network radar (Bluetooth, WiFi, Zigbee, Thread, Matter) |
| [DOCKER-MANAGER](DOCKER-MANAGER.md) | `docker-manager` | a Docker container manager, fronting `container-docker` (ADR-0095) |
| [DROP](DROP.md) | `drop` | a presigned direct-upload drop-box |
| [ESHOP](ESHOP.md) | `eshop` | eShopOnDapr recreated on wasmCloud |
| [EXPERIMENT](EXPERIMENT.md) | `experiment` | context-based A/B testing, from assignment to conversion |
| [FLAGS](FLAGS.md) | `flags` | a live feature-rollout console (set a rule, watch it propagate) |
| [FREIGHT-TRACKER](FREIGHT-TRACKER.md) | `freight-tracker` | logistics and freight tracking |
| [FS-WATCHER](FS-WATCHER.md) | `fs-watcher` | a filesystem watcher, fronting `fs-watcher` (ADR-0095) |
| [GATE](GATE.md) | `gate` | a durable traffic-shaping gateway (the Golem worker patterns) |
| [GROCERY](GROCERY.md) | `grocery` | a retail store & inventory app (barcode scanning, dual-audience RBAC) |
| [HEALTH-RECORDS](HEALTH-RECORDS.md) | `health-records` | electronic health records |
| [HELPDESK](HELPDESK.md) | `helpdesk` | a mid-sized SaaS over composed capability contracts |
| [IMAGE-OPTIMIZER](IMAGE-OPTIMIZER.md) | `image-optimizer` | an image optimizer, fronting `image-optimizer` (ADR-0095) |
| [JOBS](JOBS.md) | `jobs` | a durable background-job queue (with a swappable execution backend) |
| [LAN-SCANNER](LAN-SCANNER.md) | `lan-scanner` | a LAN device scanner, fronting `lan-scanner` (ADR-0095) |
| [LMS](LMS.md) | `lms` | a learning platform (courses, auto-graded quizzes, gradebook, certificates) |
| [LOCAL-AI](LOCAL-AI.md) | `local-ai` | a local LLM front-end, fronting `llm-local` (ADR-0095) |
| [MDNS-DISCOVERER](MDNS-DISCOVERER.md) | `mdns-discoverer` | an mDNS service discoverer, fronting `mdns-discovery` (ADR-0095) |
| [MESH](MESH.md) | `mesh` | resilient upstream calls (the breaker trips, the app stays up) |
| [PASSKEY](PASSKEY.md) | `passkey` | passwordless sign-in (the phishing-resistant one) |
| [PASTE](PASTE.md) | `bin` | a paste / gist bin over a pure-compute pipeline |
| [PDF-GENERATOR](PDF-GENERATOR.md) | `pdf-generator` | a PDF generator, fronting `browser-automation` (ADR-0095) |
| [PHOTOSOCIAL](PHOTOSOCIAL.md) | `photosocial` | social photo sharing with AI critique & RBAC-gated attribute ratings |
| [PAYEES](PAYEES.md) | `payees` | a payee book with IBAN-validated bank details |
| [PIPELINE](PIPELINE.md) | `pipeline` | a reliable event pipeline (outbox → dispatch → DLQ → replay) |
| [RATELIMIT](RATELIMIT.md) | `ratelimit` | a live throttle wall (lockout + quota, watched) |
| [REAL-ESTATE-ESCROW](REAL-ESTATE-ESCROW.md) | `real-estate-escrow` | real estate escrow management |
| [REALTIME](REALTIME.md) | `pulse` | a realtime chat room, composed from capability contracts |
| [REPORT](REPORT.md) | `report` | batch CSV import → typed validate → paged report → CSV export |
| [SAGA](SAGA.md) | `saga` | a durable trip-booking saga, composed from capability contracts |
| [SCRIBE](SCRIBE.md) | `scribe` | a collaborative document editor (convergence, made live) |
| [SEARCH](SEARCH.md) | `search` | faceted search-as-you-type over a real corpus |
| [SMART-HOME](SMART-HOME.md) | `smart-home` | IoT home automation |
| [STASH](STASH.md) | `stash` | a note stash you export as a .zip |
| [STATUS](STATUS.md) | `status` | a status page / uptime monitor |
| [STUDIO](STUDIO.md) | `studio` | components describe themselves, and compose themselves |
| [TEMPO](TEMPO.md) | `tempo` | a multi-person worktime logger (with charts) |
| [TRACK](TRACK.md) | `track` | a Linear-lite project tracker (the complex composition) |
| [TRANSIT](TRANSIT.md) | `transit` | public-transport ticketing (buy a QR, validate with a camera) |
| [VIDEO-TRANSCODER](VIDEO-TRANSCODER.md) | `video-transcoder` | a video transcoder, fronting `video-ffmpeg` (ADR-0095) |
| [VPN-MANAGER](VPN-MANAGER.md) | `vpn-manager` | a VPN manager, fronting `vpn-wireguard` (ADR-0095) |

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

