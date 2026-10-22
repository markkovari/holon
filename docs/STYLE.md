# Rust style

Rust here follows the [Google Rust Style Guide](https://google.github.io/styleguide/rustguide.html)
(itself built on the public Rust style rules and the API guidelines). Where this repo decides
differently, it says so below; anything not listed is the guide's rule. `cargo fmt` and
`clippy -D warnings` in CI are the parts a tool enforces; the rest is for review.

## Where this repo differs

- **Comments.** The guide asks for doc comments on public APIs, and in the same breath to omit
  any that repeats the name. Here the second half wins: a comment says what the code cannot
  (a reason, a limit, a platform quirk), in one line, or there is none. No `missing_docs` lint.
  A component's first `//!` line is its catalog description and stays.
- **Known limits** are marked `shortcut: <the limit>, <when to change it>` (older ones say
  `ponytail:`); they are decisions, not debt to tidy away.
- **`lock().unwrap()`** is fine: a poisoned lock means something already panicked. Any other
  `unwrap()` outside a test is `?` or an `expect("why this cannot fail")`.
- **Errors.** `Result<_, String>` is allowed where the only caller shows the message to a person
  (an agent tool, a CLI, a board). Where a caller must tell failures apart, the error is a type
  (an `enum` with `thiserror`, as `secrets-vault` and `auth-guard` do).
- **Globs.** As the guide: `use super::*` in a test module and `use Enum::*` inside a function
  about that enum. A third-party prelude a crate is built around (`gpui::prelude::*`) is allowed.
- **Imports, one module per `use`** (`imports_granularity = "Module"`): followed by hand, since
  that rustfmt option is nightly-only and CI pins stable.
- **`Arc`.** The runtime, the bridge and the hosts share state across threads; `Arc<Runtime>` and
  the like are the intended shape there, not something to design away.
