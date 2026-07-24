# scribe — collaborative document editor (SCRIBE.md)

A shared document edited by many windows at once with **no lock**: each field is
a `crdt:merge` CRDT register, merged server-side and streamed to every open
editor over SSE. See [SCRIBE.md](../../SCRIBE.md) for the full write-up.

Unlike the pure-compute `jco-*` examples, scribe is a **composed HTTP app** run
on the native Rust host (like `pulse`), so this directory holds the SPA + a Rust
e2e rather than a jco harness.

```
public/index.html        # the two-field editor SPA (EventSource + optimistic ops)
tests/scribe.rs          # e2e: concurrent-merge + out-of-order LWW + live SSE
```

## Run

```bash
# from the repo root:
just host-scribe          # composes scribe-domain (+ crdt + records + ids),
                          # serves the SPA on http://127.0.0.1:3037
```

Open **two** browser windows on that URL and type in both — edits merge and
appear live in the other window. Add `?doc=notes&name=Alice` to pick a document
and display name.

```bash
just e2e-scribe           # the convergence + live-SSE e2e (spawns the host)
```
