# `testdata/` from cucumber/gherkin

These files are vendored from
[`cucumber/gherkin`](https://github.com/cucumber/gherkin) `testdata/`, MIT licensed,
copyright the Cucumber contributors. They are not ours and are not edited.

`good/` holds files the reference implementation parses. `bad/` holds files it
refuses. `../corpus.rs` asserts the only claim that matters across the boundary:

- **no file in `good/` produces an `error`** — a warning or a `declined` is fine, an
  error means we disagree with cucumber about whether a file is valid;
- **every file in `bad/` produces at least one `error`**.

## Why they are vendored rather than fetched

A test that downloads its own fixtures fails when a website moves, and a test that
fails for that reason gets disabled. `docs.rs` in the reconciler already declines to
check URLs for the same reason.

## What is left out

`good/very_long.feature`, which is 43 KB of repetition — five sixths of the corpus by
size, and it exercises length rather than any rule the other forty-nine do not.

## What this corpus corrected

Running it against the first version of this component found six things, all of them
mistakes here rather than in the corpus:

| what | how it showed up |
|---|---|
| `*` is a step keyword, not a continuation | four good files flagged, incl. `star-keywords.feature` |
| a plain `Scenario:` with `Examples:` IS an outline | three good files flagged |
| a scenario with no steps parses fine | `incomplete_scenario_outline.feature` is *good* |
| only what is between pipes is a cell | `extra_table_content.feature` says so in its own description; and two bad files went undetected without it |
| a tag line may end in a comment | `tags.feature` has `@comment_tag1 #a comment` |
| an unknown dialect is a broken header, not an unsupported one | `invalid_language.feature` is `#language:no-such` |
