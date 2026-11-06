# vcs-store: the code store's wire

`vcs-store` (this component) exports `holon:vcs/code-store` and `holon:vcs/files`
(`wit/vcs/vcs.wit`) and runs the `holon-vcs` engine in-process over this app's
keyvalue, moved by `comp:store/cas`
([ADR-0134](../../docs/adr/0134-the-code-store-is-a-component.md)). Each call goes
through `holon_vcs::serve::dispatch`, the JSON below, so a refusal reaches a caller
as the same `vcs-error` case whichever side it came in by. `vcs-gateway` serves the
same routes, with the same JSON, over `comp-host`; `comp-agentd` calls them there.

The JSON is defined once, in `crates/holon-vcs/src/wire.rs`.

## Storage

One keyvalue bucket (`default`) holds everything, so `kv = "sqlite"` or `kv = "nats"`
makes it durable:

| keys | what |
|---|---|
| `blob/<sha-256>` | content, written once |
| `oplog/<ws>/…` | the write-ahead op log |
| `ws/<ws>/…` | each symbol's tip and name claims (`KvPointers`): the commit point |
| `graph/<ws>/{sym,patch,conflict}/<digest>` | one record each |
| `graph/<ws>/{component,named,dependents,conflicts,key-conflicts}/…` | index lists that only grow; every lookup reads the records and filters |

The graph is an index the oplog can rebuild (`repair`).

## Routes

Every route is `POST /v1/<WIT function name>` with one JSON object, and answers
the function's `ok` value as the JSON body with `200`.

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

An `ingest-patch` is `{symbol, edit, outcome}` with `edit` one of `create`,
`replace`, `delete`, `rename`, `move` and `outcome` either `{"ok": commit-result}`
or `{"err": error-body}` — a refusal of one symbol does not fail the ingest.

The gateway additionally answers `GET /` and `GET /v1` with the route list and
`GET /health` with `{ok: true}`.

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
| `not-permitted` | the `dest` | 403 | `invalid("not-permitted: …")` — `materialize` outside the host's `--dir` |

Anything else — a keyvalue error, a record that is not this JSON, an `error` this
side does not know or whose `detail` does not have its case's shape — is `storage-error` with what happened
in its text. That is the one case the contract says to retry, and it is the
right one for "the store did not answer".

## Rules

- **Paths.** `ingest-*` paths are component-relative, `/`-separated, with no
  empty, `.` or `..` segment (`invalid` otherwise, nothing written).

## Rules

- **Paths.** `ingest-*` paths are component-relative, `/`-separated, with no
  empty, `.` or `..` segment (`invalid` otherwise, nothing written).
- **`materialize`** writes under a directory the host was started with `--dir`
  for (the manifest's `dirs`), seen by the guest at the same path; nothing else is
  reachable, so outside one the parent does not exist for it and the call is
  `not-permitted`. `dest` must be absolute, is created if missing and refused
  (`invalid`) if it exists and is not an empty directory. It writes exactly the
  snapshot's entries and nothing else, so `git write-tree` of `dest` is the
  snapshot's `git-tree` — except an executable entry: WASI has no chmod, so it is
  written `0644` while the snapshot (and its tree) still say executable.
  Open conflicts refuse it (`unresolved-conflict`) like `snapshot-export`.
- **Bodies** up to 64 MiB at the gateway; the guest's memory is the real ceiling,
  which is why `apps/vcs.toml` sets `mem_cap_mb = 256`.

## Crashes

An op is written ahead to the oplog, its tip moved by compare-and-set (the commit
point), then finished. A guest that traps half-way leaves the op pending; the next
call that meets it settles it — rolled forward if its commit point happened,
aborted once it is older than the lease (30 s) if it can no longer happen. Inside
the lease it is presumed in flight: `verify` lists it under `in-flight`, and
`snapshot-export` answers `concurrent-modification` until a reader past the lease
fences it. `repair` does the same for a whole workspace on demand. The engine's
suite crashes every step of the write order over these stores
(`crates/holon-vcs/tests/kv.rs`).
