# search — faceted search-as-you-type

A live search box for the **search** capability (see [`docs/apps/SEARCH.md`](../../SEARCH.md)):
type and ranked results narrow with every keystroke, click a **facet** to filter,
and watch the **cache hit-ratio** climb on repeat queries.

One composed wasm HTTP component (`search-domain` + `search-index` +
`record-store` + `cache` + `metrics-collect` + `pagination`) on the native Rust
host. The engine is the `search:index` contract (TF-IDF over a KV inverted
index); the corpus is `records:store`; the cache + hit-ratio are `cache:store` +
`metrics:collect`.

## Run it

```bash
just host-search           # compose + serve on http://127.0.0.1:3019
```

Open the page (it seeds a 10-doc corpus on load):

1. Type `saga`, `wasm`, `encryption` — ranked hits appear, TF-IDF score shown.
2. Toggle **any/all** — `all` intersects terms (fewer, sharper hits).
3. Click a facet chip (`kind:doc`, `topic:reliability`, …) to filter.
4. Repeat a query — it comes back **⚡ cached** and the hit-ratio ticks up.

## Test it

```bash
just e2e-search            # ranking, all-mode intersection, facet filter, cache hit
```

The e2e seeds the corpus and asserts a rare term ranks its doc first, all-mode
shrinks the result set, a tag facet restricts hits, and an identical repeat
query is served from cache (hit-ratio positive).
