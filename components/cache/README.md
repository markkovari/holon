# cache:store — generic TTL cache + four strategies

A small, reusable cache capability (its own WIT package, like `ratelimit:guard`).
Values are opaque bytes; TTL is enforced in-value over `wasi:keyvalue` (the store
has no native expiry). The strategy operations that touch a backing store call
back into a `source` (read) / `sink` (write) the consumer provides — so the
component both **exports** `cache` and **imports** `source`+`sink`, wired with
`wac`.

## Interface (`cache:store/cache`)

Primitives: `get` · `set(key,val,ttl)` · `peek` · `delete`/`invalidate` ·
`invalidate-prefix` · `ttl`.

Strategies:

| op | strategy | behavior |
|----|----------|----------|
| (consumer code) | **Cache-Aside** | consumer does `get` → on miss `load` + `set`. No callback. |
| `get-through` | **Read-Through** | on miss, cache calls `source.load`, stores, returns |
| `put-through` | **Write-Through** | writes `sink.store` then cache, synchronously |
| `put-behind` + `flush` | **Write-Behind** | cache now + enqueue; `flush` drains to `sink` (no background tasks in wasip2, so flush is explicit) |

## Backing callbacks (imported)

```wit
interface source { load: func(key) -> result<option<list<u8>>, string>; }
interface sink   { store: func(key, value) -> result<_, string>;
                   remove: func(key) -> result<_, string>; }
```

The consumer (or another component) provides these; the cache invokes them for
the through/behind strategies. Cache-Aside and the primitives need neither.

## Try it

`../../examples/jco-cache` transpiles this component with jco and a fake backing
store, and exercises every primitive + all four strategies:

```bash
cd ../../examples/jco-cache && npm install && npm test   # 10/10
```

## Notes

- Keys are namespaced/sanitized to NATS-legal chars (`c_…`); write-behind
  markers live under a separate `wb_` namespace.
- Write-through writes the backing store FIRST and only caches on success, so
  the cache never holds a value the sink rejected.
- `flush` retains entries whose `sink.store` fails, for the next call.
