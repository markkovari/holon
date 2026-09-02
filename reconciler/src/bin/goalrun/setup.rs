//! Everything a run sets up before it spends anything.
//!
//! Extracted from `main`, which was 779 lines and is mostly this: the caches a
//! gate will read, the environment that points every check at them, and the
//! fixtures the fleet is started from. None of it decides anything — it is the
//! part of the run that would be identical if the goal were different.

use std::process::Command;

use anyhow::{bail, Result};
use comp_reconciler::gate::{egress_authority, Gate};

use crate::{
    artifacts, host_bin, plug_bin, render, warm_the_gate_cache, Args, GateCaches, GoalSpec,
};

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
        &goal,
        &args,
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
