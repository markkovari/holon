# vcs-store ↔ comp-vcs contract

`vcs-store` (this component) exports `holon:vcs/code-store` and `holon:vcs/files`
(`wit/vcs/vcs.wit`) and implements every call as ONE HTTP request to `comp-vcs`
(native, `reconciler/src/bin/vcs.rs`), which runs the `holon-vcs` engine over NATS
JetStream and SurrealDB. Why the split: [ADR-0099](../../docs/adr/0099-an-agent-native-code-store.md),
*The service*, and [ADR-0095](../../docs/adr/0095-what-is-allowed-to-be-native.md).
`vcs-gateway` serves the same routes, with the same JSON, to agents over
`comp-host`; this file is the wire for both.

The JSON is defined once, in `crates/holon-vcs/src/wire.rs`, and every side —
daemon, store component, gateway — uses that definition.

## Config the component reads (`wasi:config`)

| key | meaning |
|---|---|
| `vcs-url` | where `comp-vcs` listens, e.g. `http://127.0.0.1:8014` (a base path is kept) |
| `vcs-token` | bearer token; must equal the daemon's `--token` (absent or empty = no header) |

## Routes

Every route is `POST /v1/<WIT function name>` with one JSON object, and answers
the function's `ok` value as the JSON body with `200`. Every route except
`GET /health` requires `Authorization: Bearer <token>` when the daemon has one
(`comp_reconciler::daemon_auth`); a wrong or missing one is a bare `401`.

Records are the WIT's, field for field, kebab-case (`holon_vcs::model`): a
variant is `{"case": payload}` or `"case"` when it has none
(`{"create": {"inline": "fn f() {}\n"}}`, `"delete"`, `{"after": {symbol-id}}`),
an enum is its case name (`"function"`, `"open"`), an `option` is the value or
`null`, and an absent `option` field is `null`. Bytes are base64 strings.

| route | body | `200` body |
|---|---|---|
| `POST /v1/apply-patch` | `patch-request` | `commit-result` |
| `POST /v1/resolve-conflict` | `resolution-request` | `commit-result` |
| `POST /v1/revert-op` | `{workspace, op, by: agent}` | `op-entry` |
| `POST /v1/query-symbol` | `{workspace, query: symbol-query}` | `[symbol-view]` |
| `POST /v1/snapshot-export` | `{workspace, component}` | `snapshot` |
| `POST /v1/list-conflicts` | `{workspace, state?: conflict-state}` | `[conflict]` |
| `POST /v1/oplog` | `{workspace, after?: op-id, limit}` | `[op-entry]` |
| `POST /v1/oplog-head` | `{workspace}` | `op-id` (a bare number) |
| `POST /v1/verify` | `{workspace}` | `consistency-report` |
| `POST /v1/repair` | `{workspace}` | `repair-report` |
| `POST /v1/ingest-file` | `{workspace, component, file: {path, content}, by: agent, read-at?}` | `ingest-report` |
| `POST /v1/ingest-tree` | `{workspace, component, files: [{path, content}], by, read-at?, prune?}` | `[ingest-report]` |
| `POST /v1/materialize` | `{workspace, component, dest}` | `{dir, snapshot, files, bytes}` |
| `POST /v1/read-blob` | `{blob: hash}` | the bytes, base64 (a JSON string) |
| `GET /health` | — | `{ok, backend, pointers, graph, lease-ms, workspaces, startup-repair, fault-injection}` |

An `ingest-patch` is `{symbol, edit, outcome}` with `edit` one of `create`,
`replace`, `delete`, `rename`, `move` and `outcome` either `{"ok": commit-result}`
or `{"err": error-body}` — a refusal of one symbol does not fail the ingest.

The gateway additionally answers `GET /` and `GET /v1` with the route list and
`GET /health` with `{ok: true}` (the component's own health, not the daemon's).

## Errors

Not `200`: the body is

```json
{ "error": "<case>", "detail": <payload>, "message": "<for people>" }
```

`error` is the `vcs-error` case; `detail` its payload as JSON; `message` is never
parsed. **Status codes are used, and are informational**: a client decides by
`error`, never by the status alone.

| `error` | `detail` | status | component receives |
|---|---|---|---|
| `storage-error` | string | 503 | `storage-error(detail)` — retry may help |
| `concurrent-modification` | `cas-failure` | 409 | `concurrent-modification(detail)` — re-read and retry |
| `unresolved-conflict` | `[conflict-id]` | 409 | `unresolved-conflict(detail)` — resolve first |
| `name-taken` | `symbol-id` | 409 | `name-taken(detail)` — a retry can never succeed |
| `symbol-not-found` | `symbol-id` | 404 | `symbol-not-found(detail)` |
| `not-found` | string | 404 | `not-found(detail)` |
| `invalid` | string | 400 | `invalid(detail)` |
| `bad-request` | string | 400 | `invalid("bad-request: …")` — the body was not the route's JSON |
| `not-permitted` | the `dest` | 403 | `invalid("not-permitted: …")` — `materialize` outside `--allow-path` |

Anything else — no `vcs-url`, nothing listening, the daemon dying mid-request, a
`401`, a body that is not this JSON, an `error` this side does not know or whose
`detail` does not have its case's shape — is `storage-error` with what happened
in its text. That is the one case the contract says to retry, and it is the
right one for "the store did not answer".

## Rules

- **Paths.** `ingest-*` paths are component-relative, `/`-separated, with no
  empty, `.` or `..` segment (`invalid` otherwise, nothing written).
- **`materialize`** writes under a directory the daemon was started with
  `--allow-path` for (repeatable; none = every call `not-permitted`). The parent
  of `dest` must exist and be inside an allowed directory once both are
  canonicalised, so `..` and a symlinked parent cannot walk out; `dest` is created
  if missing and refused (`invalid`) if it exists and is not an empty directory.
  It writes exactly the snapshot's entries (mode `0644`, `0755` for executables)
  and nothing else, so `git write-tree` of `dest` is the snapshot's `git-tree`.
  Open conflicts refuse it (`unresolved-conflict`) like `snapshot-export`.
- **Bodies** up to 64 MiB (an `ingest-tree` of a component, base64'd).
- **Timeouts.** The component allows five minutes per call.

## Startup and crashes

Before it listens, `comp-vcs` runs `repair` on every workspace whose oplog is in
its bucket (`--bucket-prefix`): an op the previous process left half-done is
rolled forward if its commit point (the tip compare-and-set) happened, and aborted
if it can no longer happen — which, for an op whose pointer nothing else moved, is
once it is older than the lease (`--lease-secs`, default 30). Inside the lease it
is presumed in flight: `verify` lists it under `in-flight`, and `snapshot-export`
answers `concurrent-modification` until a reader past the lease fences it.
`GET /health` lists what startup repair did per workspace.

`--crash-after-step <step>[:<n>]` / `--crash-before-step <step>[:<n>]`, with
`--i-know-this-is-a-test`, abort the process at the `n`-th write of a step of the
write order (`blob`, `intent`, `graph`, `claim`, `tip`, `finish`, `release`,
`commit`), counted from the first request after startup. For the e2e suite only;
the daemon refuses to start with either flag alone.

## Flags

| flag | default | |
|---|---|---|
| `--addr` | `127.0.0.1:8014` | |
| `--token` / `--token-file` | none | bearer secret (`daemon_auth`) |
| `--nats-url` (`VCS_NATS_URL`) | `nats://127.0.0.1:4222` | JetStream: blobs (ObjectStore), pointers and oplog (KV) |
| `--bucket-prefix` | `holon-vcs` | buckets `<prefix>-blobs`, `-pointers`, `-oplog` |
| `--surreal-url` (`VCS_SURREAL_URL`) | `127.0.0.1:8000` | the graph, over WebSocket |
| `--surreal-ns`, `--surreal-db` | `holon`, `vcs` | |
| `--surreal-user`, `--surreal-pass` / `--surreal-pass-file` | `root`, `root` | the compose dev credentials; set real ones |
| `--memory` | off | every store in memory, nothing durable |
| `--lease-secs` | `30` | fractions allowed |
| `--allow-path` | none | where `materialize` may write, repeatable |
