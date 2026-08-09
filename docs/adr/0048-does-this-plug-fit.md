
# 0048 — Does this plug fit?

Status: accepted. Completes the deploy-time validation ADR-0005 and the original plan
described, alongside ADR-0047.

## The gap

Save-time validation answers "is every import satisfied by *something named right*".
`composer::plan` matches interface names, and that is the correct test for planning a
graph — but it is the weaker one. Two components can both talk about
`records:store/store@0.1.0` and still not fit: the version, the record fields, a
resource's methods all have to line up, and `wac plug` applies a real subtype check
that a name comparison cannot.

The stronger check already existed. `composer::satisfies(socket, plug)` — wac's own
test, on the real bytes — has been in the reflect world all along with nothing calling
it.

## The endpoint

```
POST /api/components/satisfies {socket, plug}
```

Measured against the components the linked e2e fixture wires together:

```
record-store into gate   fits: true   ["records:store/store@0.1.0"]
shaper into gate         fits: true   ["shaper:limit/limiter@0.1.0"]
gate into record-store   fits: false  []
    "`gate` exports nothing that `record-store` imports — matching interface
     NAMES is not enough, the types have to fit too"
```

**It belongs at edge-draw time**, which is why it is a separate endpoint rather than
more work at save. The answer is wanted while someone is dragging a line between two
boxes; a UI that can only find out at save is a UI that lets you build something
invalid and then explains why afterwards.

**The reply lists every interface the plug would satisfy, not just the one asked
about.** `wac plug` matches *every* common interface between a plug's exports and the
socket's imports and cannot be told to satisfy only one — the composer's own
`also-satisfies` field says so. A UI that draws one edge while three were actually
wired is a UI that lies, and this is the data that stops it.

**Visibility is the deploy rule, unchanged**: own row first, then anything `may_use`
allows. Asking whether two components fit must not become a way to discover that a
private component exists.

## Why the weaker check stays

`plan`'s name matching is still what runs at save, and it should be. It works on the
`surface` already stored on each catalogue row, so it costs no bytes and no wac
invocation, and it is what produces the gap list with candidates. The subtype check
needs both components' actual bytes — fine for one edge in an editor, wasteful for
every edge of every graph on every save.

Two tests, one cheap and always-on, one exact and on demand.

## Request bodies got the same treatment

Asked while this was being written: if `comp.toml`, the app spec and component config
all refuse a misspelled key, why does the API not? It did not — handlers read
`b["name"]` and an unknown field simply did nothing:

```
POST /api/deployments {"name":"shop","noodes":[{"id":"gate"}]}
  -> 201, a deployment with an EMPTY graph
```

The most expensive shape of the bug is the save path, where `{"noodes": [...]}` saved
the old graph and reported success. `deny_unknown_fields` on a typed body is the whole
fix, and serde already writes the message by hand:

```
unknown field `noodes`, expected one of `name`, `strategy`, `nodes`, `edges`
unknown field `stratgey`, expected one of `name`, `strategy`, `nodes`, `edges`
```

422 rather than 400: the JSON parsed fine, it just said something the endpoint does
not accept, and a 400 would tell a client its serialiser is broken.

## What is still missing

- **Nothing calls it yet.** The endpoint exists and is correct; the studio UI that
  should call it on edge draw is not built (ADR-0011). Until then this is a facility,
  not a feature — worth saying plainly rather than counting it as done.
- **It answers about a pair, not a graph.** Whether a whole canvas composes is still
  `plan`'s job, and `compose` is the final proof for a fused build.
- **No caching.** Each call fetches both components and runs wac. Fine at editor
  pace; a UI that calls it on every mouse-move would need debouncing on its side.
