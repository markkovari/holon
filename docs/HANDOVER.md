# Handover — 2026-10-24

State of the repository and of the one machine it runs on, for whoever (person or agent) picks it
up next. How to work in the repo: [`AGENTS.md`](../AGENTS.md). This file goes stale; check it
against `git log` and `gh pr list` first.

## Where it stands

- `main` is green and nothing is open: every PR through #450 is merged and every local branch is
  gone.
- CI runs nineteen jobs, the heavy ones on picur's self-hosted runners (`picur-1`, `picur-2`).
  Includes formatting, clippy, test suites, checks, component gates, and e2e (playwright). All passed.

## The live system (picur, macOS, launchd)

`infra/launchd/install.sh` writes and starts these as `io.holon.<name>`. Logs are in
`~/Library/Logs/holon/<name>.log`. `infra/launchd/rebuild.sh` rebuilds the runtime.

| service | listens | notes |
|---|---|---|
| `nats` | :4222 | JetStream; the platform vault and the code store keep state in it |
| `platform` | :3936 | `comp-host` running `platform-domain`: the vault agents' API keys live in |
| `vcs` | :18014 | comp-vcs, the code store (ADR-0099), on NATS + the `holon-surreal` Docker container (:8000) |
| `agentd` | :18016 | coding sessions (ADR-0102) on DeepSeek; each `write_file` is recorded in `vcs` (ADR-0121) |
| `runtime` | :18017 | agent-runtime, the agents; its API is `/v1` (ADR-0120) |
| `bridge` | — | agent-matrix, the agents in Element |
| `fm`, `embed`, `qwen`, `qwen-large` | 18099–18103 | local models; the qwen ones need `mlx_lm`, which is not installed |

- **Tokens** are in `~/.holon-agents/` (`admin-token`, `agentd-token`, `vcs-token`).
- **Secrets** are in `~/.comp-secrets/` and the platform vault. Do not read either.
- **The native board** is `gpui-console`; `gpui-console/install.sh` builds and installs it.
- **Rebuilt & deployed:** `gpui-console`, `agent-runtime`, `agent-matrix`, and `comp-agentd` were rebuilt
  and reloaded following #443-#450.

## Done recently (Oct 22–24)

| what | PRs |
|---|---|
| Coding-session edits in the code store, with rewind; deployed | #418, #419 |
| CI on picur's self-hosted runners; `target/` folders outside the checkout | #422, #428, #438 |
| comp-agentd sessions are a state machine replayed from their log (`machine.rs`) | #424 |
| agent-runtime `/v1/state` is a view folded from the event stream (`view.rs`) | #425 |
| The goal search's stop rules in one place | #426 |
| Reconciler: lease held through a pass; only the leader activates; plug replicas; lag | #427, #431 |
| Domains refuse illegal status moves (`guestauth::may_move`) | #429 |
| Roles-by-email limited to test tokens; `/test/token` behind `test-tokens = on` | #432, #441 |
| goal runs: an unlanded run is not handed to a human; reruns; `pr-opened` | #433, #444 |
| The board shows approvals for runs and sessions already going when it connects | #434, #443 |
| fsm domains: one source of status (ecommerce, dispatch, triage) | #435, #439, #442 |
| Approval timeouts are not "no"; approval ids survive restarts | #443, #425 |
| Component gates and the Playwright suite run again, and in CI | #438 |
| Documented repo conventions in `AGENTS.md` and handover notes; CI bash fix | #445 |
| Reconciler & relay: release leader lease on SIGINT and SIGTERM via `ShutdownListener` | #446 |
| goald: avoid duplicate PRs across crashes and retry review posting | #447 |
| agent-runtime: state safety for `archive_project`, `remove_key`, and `grant_key` | #448 |
| agent-matrix: paginate runs when delivering scheduled results | #449 |
| comp-agentd: hold session and conversation lock during rewind; evaluate retry deadline | #450 |

## Open, not started

Each item below was found by reading the code. Where a bug has not been reproduced, it says so.

**agent-runtime** (`agent-runtime/src/`)
- A run still has no `Running` status (`crates/agent-wire` `Status`). Adding one touches about 80
  match sites in the board and the bridge.
- An agent's state is spread out: a `paused` bool, a trash folder and a project's
  `paused_by_archive` list. One state enum would replace them.
- HTTP status codes are chosen by matching error text (`server.rs`, `v1.rs`), and the old non-`/v1`
  routes duplicate `/v1` (`server.rs:144`).
- `last_fired` is keyed by trigger index, so editing an agent's triggers shifts it. Timers are taken
  out before they run, so a crash loses them (`runtime.rs`).

**comp-agentd** (`reconciler/daemons/agentd/`)
- Edits made through the `run` tool are not recorded in the code store. A rewind does not rewind
  the conversation.

**Reconciler** (`reconciler/`)
- `plan()` is about 260 lines (`src/plan.rs:336`), and goalrun's `run` and `decomposed` duplicate
  each other's recording and landing (`goal/src/bin/goalrun.rs`). Both are plain extraction work.

**Components**
- 22 scaffold domains hand-copy their `Reply`, `Route`, `now_secs`, `mint` and `authorize_perm`
  helpers instead of using `guestauth`.
- Something in the components workspace turns on serde_json's `preserve_order`. bytes-codec's gate
  now compares JSON with sorted keys; which crate enables it was not found.
