
# 0056 — A converged app keeps its placement

Status: accepted. Extends [ADR-0055](0055-how-the-control-loop-scales.md).

## The observation that started it

Every converged cell in ADR-0055 emitted **zero commands**, and the worst of
them took 1.23 s to do it. A reconciler's normal condition is having nothing to
do, so re-ranking every node in the fleet for every app — to rediscover a
placement that was already correct — is the wrong shape for the common case.

## The change

`settled()` returns the placement an app already has, from the owner-keyed index
ADR-0055 built, instead of ranking anything.

It cannot simply *skip* the app. The diff reads a key present in `have` and
absent from `want` as "stop it", so a skipped app would be torn down. The
existing placement has to flow through the rest of the loop unchanged —
including the stateful-spread check and the linked components that follow the
root onto the same nodes.

```
 nodes   apps  insts │  cold ms  steady ms
    10  10000  20000 │    40.07     24.88
  1000  10000  20000 │  1226.54     45.98
```

**27× on the worst cell.** Two passes are now distinguished: the cold pass after
a restart or a fleet change, which still ranks everything, and the steady pass,
which is what the loop actually spends its life doing.

The benchmark had to be fixed to see this at all — it built a fresh
`Hysteresis` per run, so it only ever measured a cold pass, and the fast path
could never engage. A benchmark that resets the state the optimisation lives in
cannot measure the optimisation.

## Distribution is the property at risk, so it is the property proven

A lookup that returns a different node from the one the ranking would have
chosen is not a visible bug. It is a slow drift into imbalance that no scenario
test would catch, and it would be blamed on something else entirely.

So the fast path is taken **only where it cannot differ**: when the app is
already one replica per node. That is the state the ranking always produces when
at least `replicas` nodes are eligible, because its first key is "replicas of
this component already here, descending" — the current holders *are* the top
slice it would pick. A **concentrated** app, two replicas on one node while
others sit free, is a placement the ranking would split, and it takes the full
path.

Two further guards:

- **A fleet fingerprint** in `Hysteresis` — node names, labels, interfaces,
  capacity, `kv_shared`. When it changes, every app takes the full ranking for
  one pass, so a joining node still attracts a share of an app that is already
  at its replica count. Instance counts are deliberately *not* in the
  fingerprint: they change constantly and would disable the fast path forever.
- **Every holding node must still `fit`.** A relabelled node is exactly when a
  placement should move, and the app itself has not changed at all.

And then the guarantee is tested rather than argued. `the_fast_path_never_places_differently_from_the_full_ranking`
runs **140 generated worlds** — node counts 1–5, replicas 1–7 so both the
one-each and the proportional branch are covered, uneven capacity, and a
concentrated start — with the fast path on and off, asserting identical
placement. `--no-fast-path` turns it off in the field, so an operator who
suspects it can eliminate it in one restart rather than one investigation.

## Bounds

- The fingerprint is a `DefaultHasher`, compared only within one process's
  lifetime. It is not stable across releases and does not need to be.
- The differential test generates worlds; it does not enumerate them. Placement
  inputs it never varies — pinned mode, daemon mode, `host_needs` mismatches —
  are covered by the older scenario tests, not by the sweep.
- Daemon placement always takes the full path: it is one per *eligible* node, so
  a new node must grow it and counting replicas cannot see that.
