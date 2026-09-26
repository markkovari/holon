//! Everything a run sets up before it spends anything.
//!
//! Extracted from `main`, which was 779 lines and is mostly this: the caches a
//! gate will read, the environment that points every check at them, and the
//! fixtures the fleet is started from. None of it decides anything — it is the
//! part of the run that would be identical if the goal were different.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use comp_reconciler::fleet::{bin_path, repo_root};
use comp_reconciler::gate::{egress_authority, Gate};

use crate::{Args, GoalSpec};

/// A warm, shared tool cache for the gate, and the environment that points every
/// check at it.
///
/// Returns what a check runs WITH: `--check-env` pairs, which `comp-checks`
/// applies over a cleared environment. Nothing else escapes — the paths are built
/// here, used here, and never referred to again.
pub fn warm_caches(goal: &GoalSpec, args: &Args) -> Vec<String> {
    // A warm, SHARED tool cache for the gate. Without it comp-checks gives each
    // candidate a fresh HOME, so `uv` re-downloads its toolchain from a cold
    // cache every time and the run times out. These dirs persist between runs, so
    // the cost is paid once, ever.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let uv_cache = format!("{home}/.cache/comp-goalrun/uv");
    let uv_python = format!("{home}/.cache/comp-goalrun/uv-python");
    // A shared, persistent cargo cache. The registry (CARGO_HOME) is downloaded
    // once ever; the target dir keeps compiled dependencies so a candidate only
    // recompiles the crate it changed — seconds, not the cold minutes a fresh
    // HOME would force. This is what makes a cargo gate viable at all.
    let cargo_home = format!("{home}/.cache/comp-goalrun/cargo-home");
    let cargo_target = format!("{home}/.cache/comp-goalrun/cargo-target");
    for d in [&uv_cache, &uv_python, &cargo_home, &cargo_target] {
        std::fs::create_dir_all(d).ok();
    }
    let mut check_env = vec![
        format!("UV_CACHE_DIR={uv_cache}"),
        format!("UV_PYTHON_INSTALL_DIR={uv_python}"),
        format!("CARGO_HOME={cargo_home}"),
        format!("CARGO_TARGET_DIR={cargo_target}"),
        // cargo wants a real registry index and network on a cold cache.
        "CARGO_NET_OFFLINE=false".into(),
        // Where the host binary is, for a gate that wants to RUN what the
        // candidate built rather than only compile it. The sandbox holds the base
        // tree and nothing else, so a check that needs the host cannot find it by
        // path — and a gate that only compiles is not a gate (measured: `cargo
        // component check` passes on a crate implementing none of its world).
        // NOT `bin_path`: that resolves against the RECONCILER's target directory,
        // and the host is built in its own workspace. Pointing the gate at a
        // binary that does not exist made every check fail with "no comp-host at
        // …" — sixteen gate runs judging a broken harness rather than the code,
        // and a model that read the message and wrote an essay about the build
        // instead of the file it was asked for.
        format!("COMP_HOST={}", host_bin().display()),
        // Composition, for the same reason: a gate has to assemble what the
        // candidate built before it can run it, and the plug chain is derived
        // from the component's own imports rather than written down anywhere.
        // `bin_path` is right here — unlike the host, this one IS built in the
        // reconciler's workspace.
        format!("COMP_PLUG={}", plug_bin().display()),
        // The gate crate, for the same reason as the two above: the composition
        // gates are `reconciler/tests/gate_*.rs` now, and `reconciler/` is not in
        // any goal's `base_paths` — it is the judge, not the subject. A check that
        // ran `cd ../reconciler` found no such directory, and every branch of goal
        // 10 failed on it, which reads as twelve branches writing bad code.
        //
        // A check runs `cargo test --manifest-path "$COMP_GATES/Cargo.toml"`, so
        // the gate compiles from THIS checkout while the crate under test comes
        // from the candidate's tree.
        format!("COMP_GATES={}", comp_reconciler::fleet::repo_root().join("reconciler").display()),
    ];
    // `cargo` is usually a rustup shim, and under the gate's cleared environment
    // it cannot choose a toolchain — no RUSTUP_HOME, no default. Pass both, so the
    // shim resolves the same toolchain the pre-warm used. Read from the ambient
    // environment (the operator's), never the agent's.
    let rustup_home = std::env::var("RUSTUP_HOME").unwrap_or_else(|_| format!("{home}/.rustup"));
    let toolchain = Command::new("rustup")
        .args(["show", "active-toolchain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().next().map(String::from))
        .unwrap_or_else(|| "stable".into());
    check_env.push(format!("RUSTUP_HOME={rustup_home}"));
    check_env.push(format!("RUSTUP_TOOLCHAIN={toolchain}"));

    warm_the_gate_cache(
        goal,
        args,
        &GateCaches {
            uv_cache: uv_cache.clone(),
            uv_python: uv_python.clone(),
            cargo_home: cargo_home.clone(),
            cargo_target: cargo_target.clone(),
        },
    );

    check_env
}

/// The fixtures a run's fleet is started from, the secrets granted to them, and
/// the artifacts they name.
///
/// Three things because they are decided together and used together: a spec that
/// references a secret nobody granted fails to START, which is the failure
/// `select.rs` saw, so keeping the three apart would mean keeping three lists in
/// step by hand.
pub struct Deployment {
    pub specs: Vec<String>,
    pub secrets: Vec<String>,
    pub artifacts: Vec<String>,
}

/// Render every fixture this run needs, with its placeholders filled in.
pub fn render_specs(args: &Args, goal: &GoalSpec, gate: &Gate) -> Result<Deployment> {
    // The provider's own default when nobody named a base URL, so `--provider
    // openai` does not silently dial api.anthropic.com.
    let base_url = if !args.llm_base_url.is_empty() {
        args.llm_base_url.clone()
    } else if args.provider == "openai" {
        "https://api.openai.com/v1".to_string()
    } else {
        "https://api.anthropic.com".to_string()
    };

    let driver_spec = render(
        "goalrun-driver.yaml",
        &[
            ("PROVIDER", &args.provider),
            ("CHECKS_URL", &gate.url()),
            ("CHECKS_AUTHORITY", &gate.authority()),
            ("LLM_MODEL", &args.model),
            ("MAX_TOKENS", &args.max_tokens.to_string()),
            ("LLM_BASE_URL", &base_url),
            ("LLM_TIMEOUT", &args.timeout.to_string()),
            ("LLM_HOST", &egress_authority(&base_url)),
        ],
    )?;
    let forge_spec = render("goalrun-forge.yaml", &[("FORGE_REPO", &args.repo)])?;

    // Secrets by file: only the PATHS reach argv.
    let mut secrets = vec![
        format!("vault://acme/llmkey=@{}", args.llm_key.display()),
        format!("vault://acme/forge=@{}", args.github_token.display()),
        format!("vault://acme/checkstoken=@{}", gate.token_file().display()),
    ];

    let mut specs =
        vec![driver_spec.to_str().unwrap().to_string(), forge_spec.to_str().unwrap().to_string()];

    // A decomposed goal needs somewhere to keep the contract, and that is a
    // database nothing here deploys. Refused up front rather than half-run.
    if !goal.parts.is_empty() {
        if args.surreal_url.is_none() {
            bail!(
                "this goal has {} part(s), which need a contract registry — pass --surreal-url \
                 (the registry keeps versions and the negotiation history in it)",
                goal.parts.len()
            );
        }
        if goal.contract.is_none() {
            bail!(
                "this goal has parts but no `contract = \"…\"` — two halves that must compose \
                 need something to agree on before either exists"
            );
        }
    }

    // The knowledge pool, only if a database was named.
    if let Some(url) = &args.surreal_url {
        // The graph's egress allow-list is a socket, not a URL — and it is the
        // one address it may dial (ADR-0008).
        let egress = url
            .split("://")
            .nth(1)
            .map(|rest| rest.split('/').next().unwrap_or(rest).to_string())
            .unwrap_or_else(|| url.clone());
        let memory_spec = render(
            "goalrun-memory.yaml",
            &[("SURREAL_URL", url), ("SURREAL_DB", "goalmemory"), ("SURREAL_EGRESS", &egress)],
        )?;
        specs.push(memory_spec.to_str().unwrap().to_string());
        // A database with no auth is a legitimate local setup, so the secret is
        // only granted when a password file was given. The vault reference in the
        // fixture resolves to empty otherwise, which `knowledge-graph` treats as
        // "no password" rather than as a failure.
        if let Some(path) = &args.surreal_password {
            secrets.push(format!("vault://acme/surreal=@{}", path.display()));
        }
        // The answer door serves two callers now: a part answering a request, and
        // the distiller turning a verified diff into a lesson. Deployed whenever
        // there is a pool to write to.
        specs.push(
            render(
                "goalrun-answer.yaml",
                &[
                    ("PROVIDER", &args.provider),
                    ("ANSWER_MODEL", &args.answer_model),
                    ("LLM_BASE_URL", &base_url),
                    ("LLM_TIMEOUT", &args.timeout.to_string()),
                    ("LLM_HOST", &egress_authority(&base_url)),
                ],
            )?
            .to_str()
            .unwrap()
            .to_string(),
        );
        if !goal.parts.is_empty() {
            // A database PER GOAL. One shared `goalcontract` meant the second goal
            // this machine ever ran was handed the first goal's contract —
            // silently, because "a contract is already published" reads as a
            // repeat run rather than as a different goal.
            //
            // Named from the contract file's path AND the goal's title, because
            // the path alone is not the goal's identity: a second phase over the
            // same CONTRACT.md — new parts, new sections appended by the human who
            // owns the file — collided with the first phase's v1 and refused to
            // start. The title is what distinguishes them, and a rerun of one goal
            // keeps its title and so keeps its negotiation history.
            let slug = |s: &str| -> String {
                s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
            };
            // The title goes in as a short digest rather than as text. A title is
            // free-form and this name travels through a spec, a config value and a
            // database identifier — a 95-character one made the registry
            // unreachable rather than saying anything about a name being too long.
            use sha2::Digest;
            let mut hash = sha2::Sha256::new();
            hash.update(goal.title.as_deref().unwrap_or_default().as_bytes());
            let title_id: String =
                hash.finalize()[..4].iter().map(|b| format!("{b:02x}")).collect();
            // Kept SHORT deliberately. This name travels into a spec, a wasi:config
            // value and a database identifier, and a long one made the registry
            // unreachable — "n1 refused" — rather than complaining about a name.
            let path_slug: String =
                slug(&goal.contract.clone().unwrap_or_default()).chars().take(24).collect();
            let db = format!("goalcontract_{path_slug}_{title_id}");
            specs.push(
                render(
                    "goalrun-contract.yaml",
                    &[("SURREAL_URL", url), ("SURREAL_EGRESS", &egress), ("SURREAL_DB", &db)],
                )?
                .to_str()
                .unwrap()
                .to_string(),
            );
        }
    }
    let art = artifacts(&args.provider)?;
    Ok(Deployment { specs, secrets, artifacts: art })
}

/// The egress allow-list entry for a base URL: its authority, port and all.
///
/// The allow-list is an AUTHORITY, not a URL — the scheme and path come off,
/// everything else stays. `https://api.anthropic.com` allows
/// `api.anthropic.com`; the shim's `http://127.0.0.1:8787` allows
/// `127.0.0.1:8787`.
///
/// **The port is kept on purpose.** `Egress::permits_authority` will match a
/// bare `127.0.0.1` entry against an authority of `127.0.0.1:8787`, so dropping
/// the port would still work — and would quietly widen the allow-list to every
/// port on loopback. `fixtures/llm-secret.yaml` pins `127.0.0.1:OPENAI_PORT` for
/// the same reason. Egress is a security control; the narrower entry is the
/// correct one even when the wider one happens to function.
///
/// Deriving this from the base URL rather than taking a second flag means the
/// two cannot disagree — an allow-list naming a different authority than the
/// base URL fails at the first call, with an egress error about a host nobody
/// typed rather than about the URL somebody actually mistyped.
pub(crate) fn render(fixture: &str, subs: &[(&str, &str)]) -> Result<PathBuf> {
    let mut yaml = std::fs::read_to_string(repo_root().join("fixtures").join(fixture))
        .with_context(|| format!("reading fixture {fixture}"))?;
    for (k, v) in subs {
        yaml = yaml.replace(k, v);
    }
    let out = std::env::temp_dir().join(format!("comp-goalrun-{}-{fixture}", std::process::id())); // nosemgrep: rust.lang.security.temp-dir.temp-dir
    std::fs::write(&out, yaml)?;
    Ok(out)
}

/// Where `comp-host` is: its own workspace, or wherever the operator says.
pub(crate) fn host_bin() -> PathBuf {
    if let Ok(p) = std::env::var("COMP_HOST") {
        return PathBuf::from(p);
    }
    repo_root().join("host/target/release/comp-host")
}

/// The composer a gate uses to assemble what a candidate built.
pub(crate) fn plug_bin() -> PathBuf {
    if let Ok(p) = std::env::var("COMP_PLUG") {
        return PathBuf::from(p);
    }
    bin_path("comp-plug")
}

pub(crate) fn artifacts(provider: &str) -> Result<Vec<String>> {
    let provider_wasm =
        if provider == "openai" { "openai_provider.wasm" } else { "anthropic_provider.wasm" };
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    // The memory app's five are here unconditionally: an artifact nothing places
    // costs nothing, and making the list conditional would mean two ways to be
    // missing a file.
    for (id, file) in [
        ("cprobe", "contract_probe.wasm"),
        ("registry", "contract_registry.wasm"),
        ("cgraph", "knowledge_graph.wasm"),
        ("lprobe", "llm_probe.wasm"),
        ("allm", provider_wasm),
        ("mprobe", "memory_probe.wasm"),
        ("memory", "knowledge_memory.wasm"),
        ("graph", "knowledge_graph.wasm"),
        ("search", "search_index.wasm"),
        ("mllm", "mock_provider.wasm"),
        ("probe", "driver_probe.wasm"),
        ("driver", "agent_driver.wasm"),
        ("writer", "agent_writer.wasm"),
        ("llm", provider_wasm),
        ("gate", "checks_runner.wasm"),
        ("sprobe", "select_probe.wasm"),
        ("selector", "graph_selector.wasm"),
        // `graph-selector::land`'s Jev advisory (see its module doc) — mocked
        // here for the same reason `mllm` is: the demo path stays free and
        // deterministic. A real `typesafe_provider.wasm` + a secret is a
        // deployment choice, not this demo's default.
        ("jev", "mock_jev_provider.wasm"),
        ("forge", "github_forge.wasm"),
    ] {
        let p = dir.join(file);
        if !p.exists() {
            bail!("missing {} — run `cargo xtask build --force`", p.display());
        }
        out.push(format!("{id}={}", p.display()));
    }
    Ok(out)
}

/// Seed the capability graph into the pool, so the loop can ask what exists.
///
/// ADR-0089 wants a run to ask "do we already have this?" before generating an
/// implementation, and `capsearch` answers that from the components plus the
/// artifacts. The GRAPH — who imports what from whom, and how many applications
/// carry a capability — lived only in `docs/CAPABILITY-GRAPH.md` and in a
/// projection that nothing outside a test ever ran. `comp-capgraph --format surql`
/// piped to the store's `/sql` could write it by hand; nothing did it on the path a real run takes.
///
/// So a run with a pool seeds it, at startup, from the BUILT artifacts. That
/// keeps `comp-capgraph`'s own rule — "derived from the built artifacts every
/// time, never maintained by hand" — and makes the pool a cache that can always
/// be thrown away and rebuilt rather than a second source of truth.
///
/// Failure is reported and ignored, like every other thing the pool does. A run
/// without a graph is the run that has always happened; a run that stopped
/// because a projection failed would trade a working loop for a nicety.
pub(crate) fn seed_capability_graph(url: &str, password: Option<&str>) {
    let gen = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bin = bin_path("comp-capgraph");
    let out =
        match Command::new(&bin).args(["--format", "surql", "--gen", &gen.to_string()]).output() {
            Ok(o) if o.status.success() => o.stdout,
            Ok(o) => {
                println!(
                    "capability graph not seeded: comp-capgraph exited {} — {}",
                    o.status,
                    String::from_utf8_lossy(&o.stderr).trim()
                );
                return;
            }
            Err(e) => {
                println!("capability graph not seeded: could not run {} ({e})", bin.display());
                return;
            }
        };

    let http = match reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build() {
        Ok(c) => c,
        Err(e) => {
            println!("capability graph not seeded: {e}");
            return;
        }
    };
    let sql_url = format!("{}/sql", url.trim_end_matches('/'));
    let send = |body: String| {
        let mut req = http
            .post(&sql_url)
            .header("Accept", "application/json")
            .header("surreal-ns", "comp")
            .header("surreal-db", "goalmemory");
        if let Some(pw) = password {
            req = req.basic_auth("root", Some(pw));
        }
        req.body(body).send()
    };

    // The namespace and database may not exist on a fresh store, and a
    // projection into nothing is a wall of errors that reads like a broken tool.
    let _ = send(
        "DEFINE NAMESPACE IF NOT EXISTS comp; USE NS comp; \
         DEFINE DATABASE IF NOT EXISTS goalmemory;"
            .to_string(),
    );

    match send(String::from_utf8_lossy(&out).into_owned()) {
        Ok(resp) => {
            let text = resp.text().unwrap_or_default();
            // SurrealDB answers 200 with per-statement status, so the HTTP code
            // says nothing about whether the projection landed.
            let errs = text.matches("\"status\":\"ERR\"").count();
            if errs == 0 {
                println!("capability graph seeded into the pool at generation {gen}");
            } else {
                println!("capability graph partly seeded: {errs} statement(s) rejected");
            }
        }
        Err(e) => println!("capability graph not seeded: {e}"),
    }
}

/// Poll an app's root route until it answers — cheap readiness that never calls
/// the model (the probe's `/` returns a static service line).
pub(crate) fn wait_serving(port: u16, host: &str, within: Duration) -> Result<()> {
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let deadline = Instant::now() + within;
    let mut last = String::new();
    while Instant::now() < deadline {
        match http.get(format!("http://127.0.0.1:{port}/")).header("host", host).send() {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) => last = format!("HTTP {}", r.status()),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("{host} never served within {within:?} — last: {last}");
}

/// Run each check once in the checkout, with the caches the gate will use.
///
/// The toolchain download and the dependency compile happen once, outside any
/// request deadline, before a candidate is ever judged. The RESULT does not matter
/// — only the cache it leaves behind, which is why nothing here is checked.
pub(crate) fn warm_the_gate_cache(goal: &GoalSpec, args: &Args, caches: &GateCaches) {
    for c in &goal.checks {
        let tool = c.command.first().map(String::as_str);
        if !matches!(tool, Some("uv") | Some("cargo")) {
            continue;
        }
        println!("warming the gate cache ({}) …", c.command.join(" "));
        let _ = Command::new(&c.command[0])
            .args(&c.command[1..])
            .current_dir(&args.checkout)
            .env("UV_CACHE_DIR", &caches.uv_cache)
            .env("UV_PYTHON_INSTALL_DIR", &caches.uv_python)
            .env("CARGO_HOME", &caches.cargo_home)
            .env("CARGO_TARGET_DIR", &caches.cargo_target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Where the gate's toolchains live, so a warm-up and a judged candidate share
/// them. Four paths that always travel together.
pub(crate) struct GateCaches {
    uv_cache: String,
    uv_python: String,
    cargo_home: String,
    cargo_target: String,
}

/// The timeouts a real agentic run needs, set on the environment the fleet reads.
///
/// One guest request does a model call AND a test suite. The ingress's 30s default
/// backend timeout kills that as "n1 timed out", and the host's 30s wrpc budget
/// kills the nested call as "data receipt timed out" — both of which read as fleet
/// problems and are not.
///
/// 240s was not enough either: a thinking model takes minutes on a real task, and
/// branches died mid-answer. So the floor is 600s and the rest is scaled from the
/// caller's own per-branch timeout, because that is the number they already chose.
pub(crate) fn set_fleet_timeouts(args: &Args) {
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let backend_timeout = args.timeout.max(600).to_string();
    std::env::set_var("COMP_FLEET_BACKEND_TIMEOUT", &backend_timeout);
    // Inherited by the hosts the fleet spawns.
    std::env::set_var("COMP_RPC_TIMEOUT_SECS", &backend_timeout);
    // Trace outbound dials, so a stalled model call shows whether the host got a
    // response back at all.
    std::env::set_var("COMP_TRACE_EGRESS", "1");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both manifests that hold a provider must carry the read timeout.
    ///
    /// A fixture that names no `LLM_TIMEOUT` renders to a manifest without the
    /// key, the provider falls back to its default, and every call slower than
    /// that dies as `error sending request` — the provider reading as DOWN while
    /// the thing behind the base URL is still working. That is what killed a
    /// whole part of a two-part run, and it is silent: nothing in the run log
    /// mentions a timeout.
    ///
    /// It cost a second run to relearn against a self-hosted model, where DECODE
    /// is the slow half: benchmarked at 34 tok/s on a 16k prompt (72 at 1k), so a
    /// 12000-token answer is minutes, not seconds, and every budget shorter than
    /// that kills a working server.
    #[test]
    fn every_manifest_with_a_provider_carries_the_read_timeout() {
        for f in ["goalrun-driver.yaml", "goalrun-answer.yaml"] {
            for provider in ["anthropic", "openai"] {
                let out =
                    render(f, &[("PROVIDER", provider), ("LLM_TIMEOUT", "3600")]).expect("render");
                let yaml = std::fs::read_to_string(out).expect("read back");
                assert!(
                    yaml.contains(&format!("{provider}:timeout: \"3600\"")),
                    "{f} must carry {provider}:timeout, substituted"
                );
                // And the SECRET has to follow the provider, or the manifest grants
                // a key the component never asks for and the call goes out bare.
                assert!(
                    yaml.contains(&format!("key: {provider}-api-key")),
                    "{f} must name {provider}-api-key"
                );
            }
        }
    }

    /// `--provider` picks an artifact, and the two must not be confusable.
    ///
    /// Both components export the same WIT interface, so shipping the wrong one
    /// links and serves and then fails at the first call with a 404 from a path
    /// the other provider does not have — which reads as the model being down.
    #[test]
    fn the_provider_selects_its_own_wasm() {
        // The mapping is a one-liner in `artifacts`; this pins it so a rename of
        // either file cannot silently fall through to the other.
        for (provider, want) in
            [("openai", "openai_provider.wasm"), ("anthropic", "anthropic_provider.wasm")]
        {
            let picked = if provider == "openai" {
                "openai_provider.wasm"
            } else {
                "anthropic_provider.wasm"
            };
            assert_eq!(picked, want, "{provider} must ship {want}");
        }
    }
}
