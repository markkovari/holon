# Working in this repository

For any coding agent (or person) picking this up. What the project is: [`README.md`](README.md).
Its vocabulary: [`CONTEXT.md`](CONTEXT.md). Why things are the way they are: [`docs/adr/`](docs/adr/)
(index in its README). Where things stand: `main` is green, with all handover tasks completed.

## Layout

- **One root Cargo workspace** (`Cargo.toml`): the host, reconciler and its daemons, cli, lattice,
  `crates/*`, `agent-runtime`, `agent-matrix`, xtask. Separate workspaces: `components/`
  (200+ `wasm32-wasip2` components), `gpui-console/` (the native board), `providers/*`,
  `agent-runtime/gateway`, `examples/*`.
- **Contracts are written first.** WIT for components (`components/*/wit`, `wit/`); for HTTP APIs, a
  proto and its OpenAPI twin that change together: `api/holon/v1/` (comp-agentd) and
  `api/holon/runtime/v1/` (agent-runtime). A new event kind is additive: readers ignore unknown kinds.

## Build and test

```bash
cargo build --release -p holon-host -p holon-reconciler    # always name packages with -p:
cargo test -p holon-agentd                                 # a bare root build unifies features
                                                           # and drags OpenSSL into everything
cargo xtask build                    # every component, named and stamped (what CI and comp-plug use)
components/gates.sh                  # every components/*/gate.sh (needs holon-host, comp-plug, hurl)
e2e/run_all.sh [app-domain ...]      # Playwright, offline (mock model, Stripe test key: e2e/e2e.conf)
cargo test --release -p holon-reconciler --test gate_treasury # reconciler gate/fleet suites need the
                                                               # components built and nats-server on PATH
```

- The toolchain is pinned (`rust-toolchain.toml`, 1.98.0, with the wasip2 target).
- Every `target/` is a symlink into `~/.cache/holon-target/<workspace>` (`infra/targets.sh link`;
  `clean` empties it). `cargo clean` removes the link; `targets.sh link` puts it back.
- A component built by plain `cargo component build` cannot resolve a p3 world's imports: generate
  bindings with `cargo xtask bindings`, then `cargo build`, or just `cargo xtask build`.
- Scaffold domains answer `POST /test/token` only with config `test-tokens = on` (gates and e2e set it).

## Conventions

- **Commits are not signed**: `git commit --no-gpg-sign`, and `git -c commit.gpgsign=false rebase …`
  (a plain rebase signs, and signing is broken on this machine).
- **Branch, PR, squash-merge, delete the branch.** Titles read like the history:
  `fix(scope): what is now true`. The body says what was wrong, what changed, how it was checked.
- **Style**: [`docs/STYLE.md`](docs/STYLE.md). Above all: few comments, each saying what the code
  cannot (a reason, a limit); no doc comment that restates a name. A known limit is a
  `shortcut: <the limit>, <when to change it>` comment (older ones say `ponytail:`).
- **Leave WIT alone** unless the change needs it, and say so in the PR when it does.
- **No loose Python tooling.** Python belongs only to a component or app that needs it; a helper
  script is Rust (xtask, a bin) or a few lines of shell.
- **A bug fix carries the test that failed before it**, run against the old code to prove it does.
- `cargo fmt` and `clippy -D warnings` are enforced in CI, tests included.

## CI

`.github/workflows/ci.yml`. The heavy jobs run on two self-hosted runners on the Mac called picur
(`infra/ci-runner/`, Docker on OrbStack) while the repo variable `CI_RUNNER=picur` is set, and never
for a PR from a fork. `gh variable delete CI_RUNNER` sends every job back to GitHub's runners.

- The runners share one `target/` volume, so a job must never write over a binary in place. Fetch
  into `$RUNNER_TEMP`, then copy under a temporary name and rename (see "Put the fetched artifacts in
  place" in `ci.yml`), or a running job fails with `Text file busy`.
- `infra/ci-runner/install.sh install` refuses while a runner is busy; run it at most once. Killing
  a runner mid-job orphans the job, and its session blocks the new container for minutes.

## Things that cost time before

- `git pull` on main can stop on an untracked `target` symlink that main tracks; remove the link,
  pull, and `infra/targets.sh link` again.
- Never commit a `target` symlink (`.gitignore` has `**/target`; check `git status` before a
  `git add <dir>`).
- `components/console-assets/static/` is ignored but has tracked leftovers that `vite build`
  rewrites; restore them (`git checkout -- components/console-assets/static`).
- Reading secrets (`~/.comp-secrets/*`, vault contents) is off limits: pass a key *file* to a
  program, never print it.
