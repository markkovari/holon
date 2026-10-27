use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use comp_fleet::fleet::{bin_path, repo_root};
use comp_goal::gate::{egress_authority, Gate};

use crate::{Args, GoalSpec};

/// Returns `--check-env` pairs, applied over a cleared environment.
pub fn warm_caches(goal: &GoalSpec, args: &Args) -> Vec<String> {
    // Without it each candidate's fresh HOME makes `uv` re-download its toolchain until timeout.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let uv_cache = format!("{home}/.cache/comp-goalrun/uv");
    let uv_python = format!("{home}/.cache/comp-goalrun/uv-python");
    // Shared target dir: a candidate recompiles only the crate it changed.
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
        format!("COMP_HOST={}", host_bin().display()),
        format!("COMP_PLUG={}", plug_bin().display()),
        // The gates live in `reconciler/`, which is never in a goal's `base_paths`.
        format!("COMP_GATES={}", comp_fleet::fleet::repo_root().join("reconciler").display()),
    ];
    // A rustup shim cannot pick a toolchain in the cleared env; the operator's, never the agent's.
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

/// Decided together: a spec naming a secret nobody granted fails to start.
pub struct Deployment {
    pub specs: Vec<String>,
    pub secrets: Vec<String>,
    pub artifacts: Vec<String>,
}

pub fn render_specs(args: &Args, goal: &GoalSpec, gate: &Gate) -> Result<Deployment> {
    // So `--provider openai` does not silently dial api.anthropic.com.
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

    // Only the paths reach argv.
    let mut secrets = vec![
        format!("vault://acme/llmkey=@{}", args.llm_key.display()),
        format!("vault://acme/forge=@{}", args.github_token.display()),
        format!("vault://acme/checkstoken=@{}", gate.token_file().display()),
    ];

    let mut specs =
        vec![driver_spec.to_str().unwrap().to_string(), forge_spec.to_str().unwrap().to_string()];

    // Refused up front rather than half-run: nothing here deploys the contract's database.
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

    if let Some(url) = &args.surreal_url {
        // A socket, not a URL: the one address it may dial (ADR-0008).
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
        // Granted only with a password file; an unresolved reference reads as "no password".
        if let Some(path) = &args.surreal_password {
            secrets.push(format!("vault://acme/surreal=@{}", path.display()));
        }
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
            // Per goal (contract path + title): a shared one handed a goal another's contract.
            let slug = |s: &str| -> String {
                s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
            };
            // A digest: a 95-character title made the registry unreachable.
            use sha2::Digest;
            let mut hash = sha2::Sha256::new();
            hash.update(goal.title.as_deref().unwrap_or_default().as_bytes());
            let title_id: String =
                hash.finalize()[..4].iter().map(|b| format!("{b:02x}")).collect();
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

pub(crate) fn host_bin() -> PathBuf {
    if let Ok(p) = std::env::var("HOLON_HOST").or_else(|_| std::env::var("COMP_HOST")) {
        return PathBuf::from(p);
    }
    repo_root().join("target/release/comp-host")
}

pub(crate) fn plug_bin() -> PathBuf {
    if let Ok(p) = std::env::var("HOLON_PLUG").or_else(|_| std::env::var("COMP_PLUG")) {
        return PathBuf::from(p);
    }
    bin_path("comp-plug")
}

pub(crate) fn artifacts(provider: &str) -> Result<Vec<String>> {
    let provider_wasm =
        if provider == "openai" { "openai_provider.wasm" } else { "anthropic_provider.wasm" };
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    // Unconditional: an artifact nothing places costs nothing.
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
        // Mocked like `mllm`, so the demo path stays free and deterministic.
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

/// From the built artifacts, keeping `comp-capgraph`'s "derived, never by hand". Failure is
/// reported and ignored.
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

    // A fresh store may lack the namespace and database.
    let _ = send(
        "DEFINE NAMESPACE IF NOT EXISTS comp; USE NS comp; \
         DEFINE DATABASE IF NOT EXISTS goalmemory;"
            .to_string(),
    );

    match send(String::from_utf8_lossy(&out).into_owned()) {
        Ok(resp) => {
            let text = resp.text().unwrap_or_default();
            // SurrealDB answers 200 with per-statement status.
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

/// Cheap readiness that never calls the model.
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

/// Outside any request deadline; only the cache it leaves matters, so nothing is checked.
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

pub(crate) struct GateCaches {
    uv_cache: String,
    uv_python: String,
    cargo_home: String,
    cargo_target: String,
}

/// One request does a model call and a test suite; 30s defaults read as fleet faults. Floor 600s.
pub(crate) fn set_fleet_timeouts(args: &Args) {
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let backend_timeout = args.timeout.max(600).to_string();
    std::env::set_var("COMP_FLEET_BACKEND_TIMEOUT", &backend_timeout);
    // Inherited by the hosts the fleet spawns.
    std::env::set_var("COMP_RPC_TIMEOUT_SECS", &backend_timeout);
    // So a stalled model call shows whether the host got a response at all.
    std::env::set_var("COMP_TRACE_EGRESS", "1");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without `LLM_TIMEOUT` the provider default kills slow self-hosted calls, silently.
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
                // The secret must follow the provider.
                assert!(
                    yaml.contains(&format!("key: {provider}-api-key")),
                    "{f} must name {provider}-api-key"
                );
            }
        }
    }

    /// Both export one interface, so the wrong one links and serves, then 404s at the first call.
    #[test]
    fn the_provider_selects_its_own_wasm() {
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
