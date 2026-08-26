# `portfolio-value` in `cs`

One of five implementations of this WIT interface. It goes into the same binder
composition as the Rust build and is judged by the same unedited e2e.

**See [`docs/POLYGLOT.md`](../../docs/POLYGLOT.md)** for the table, the finding, and
this language's specific notes.

```bash
just e2e-binder-poly cs portfolio-value
```

**This one does not build.** It is kept as a reproduction: the bindings generate,
the implementation compiles with zero warnings, and the link fails because mono
never emits the native-to-managed thunks for the generated
`[UnmanagedCallersOnly]` exports. The full diagnosis, and the two other .NET
obstacles found on the way, are in `docs/POLYGLOT.md`.
