
# 0052 — One copy of the machine code per digest

Status: accepted. Reduces what an idle app costs a node.

> **Correction ([ADR-0053](0053-the-matrix.md)):** the "2.33 MiB per idle app" below is
> an artefact of one configuration divided by its app count. Idle RSS on a shared
> digest is FLAT — 47.5 MiB at one app, 48.4 at thirty-two — so an idle app sharing a
> digest costs ~0.03 MiB and one with its own digest costs ~2.0 MiB. The saving from
> sharing is correspondingly larger than the 27% below: 57% at 32 apps. The mechanism
> described here is right; the per-app arithmetic was not.

## The question, and the measurement that answered it

"How do we make an idle app cheaper?" — and the honest first answer was that nobody
knew what an idle app cost, only that ADR-0019 had put the marginal component at
~2.3 MiB a long time and several architectures ago.

So: sixteen apps, sixteen tenants, all deploying the **same component digest** — the
marketplace case, many tenants on one popular component.

```
empty host                     12.0 MiB
16 apps placed                 62.8 MiB     ->  3.17 MiB per idle app
```

And the host had loaded the module **sixteen times**. `Component::deserialize_file`
ran per instance start, so sixteen apps sharing one artifact held sixteen copies of
the same machine code.

## The change

A `HashMap<digest, Component>` on the agent. A `Component` is immutable machine code
and internally reference-counted, so every app on a digest shares one copy —
per-instance state lives in the `Store`, and the linker, the remotes and the route are
still built per instance, which is what makes an instance an instance.

**Dropped when the last instance on that digest stops.** Holding machine code for
something nothing runs is precisely the idle cost this was meant to remove, and the
`.cwasm` stays on disk, so coming back is a 0.3 ms load rather than a recompile.

| | before | after |
|---|---|---|
| 16 idle apps, one digest | 62.8 MiB | **49.3 MiB** |
| marginal per idle app | 3.17 MiB | **2.33 MiB** |
| module acquisition | 300 µs (disk) | **3 µs** (shared) |
| whole start, warm | 430 µs | **~80 µs** |

A 27% cut in idle cost, and a start that got 5× cheaper as a side effect.

## What I got wrong on the way, twice

**The first patch invented a function** that would have skipped the linker entirely —
caught by the compiler, but it was the wrong shape: only *loading* is shareable, and
everything after it is per-instance.

**The second patch read the cache and never populated it.** The measurement came back
unchanged — 64.1 MiB, still 3.26 MiB per app — and the temptation was to conclude "the
module was not the cost". It was, and the log said so once it distinguished
`shared` from `cache-load` from `compile`: **15 cache-load, 0 shared**. A cache that is
never written looks exactly like a cache that does not help.

The lesson is the same one this project keeps paying for: instrument the mechanism,
not the outcome. "RSS did not move" was true and told me nothing about why.

## What the remaining 2.33 MiB is, and is not

It is **not** pooling — the pooling allocator is opt-in (`--pool`) and was off for
every number here. It is not the compiled module, which is now shared. What is left is
per-instance: the `InstancePre`/`ProxyPre` built from component plus linker, the
`Scope`, the route entry, and for a linked app a wRPC client per import.

The next lever is the `InstancePre`, and it is a real one: two *fused* apps on the same
digest have identical linkers, so their pre-instantiation state is identical too and
could be shared on `(digest, linker shape)`. That is a bigger change than a map, and it
wants its own measurement before anyone believes a number for it.

## Bounds worth stating

- The cache is unbounded in the number of **distinct digests** a node has running.
  That is bounded by what is actually placed there, and it is freed on stop, so it
  tracks the fleet rather than growing forever.
- Sharing is per node, not per fleet. Ten nodes running one popular component hold ten
  copies, one each, which is the correct answer — machine code is not shareable across
  machines.
- Everything here is one 0.4 MB component. A larger one has more machine code to share
  and the saving should grow with it, but that is an expectation, not a measurement.
