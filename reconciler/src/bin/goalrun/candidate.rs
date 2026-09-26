//! Building the base tree a candidate is judged against, materialising what a
//! candidate wrote, and the two guards that decide whether a gate is even
//! capable of judging it before anything is spent.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use comp_reconciler::compose;
use comp_reconciler::contract::Registry;
use comp_reconciler::gate::Gate;
use serde_json::{json, Value};

use crate::setup::wait_serving;
use crate::{Args, GoalSpec};

/// The tracked files the gate materialises and runs its checks over — the
/// source, the tests, and whatever the build needs (`pyproject.toml`, `uv.lock`,
/// `Cargo.toml`). Scoped to `base_paths` when given, so a goal against one crate
/// of a large repo ships that crate and its path-deps, not the whole tree.
pub(crate) fn base_tree(checkout: &Path, base_paths: &[String]) -> Result<Vec<Value>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["ls-files"])
        .output()
        .context("git ls-files")?;
    if !out.status.success() {
        bail!("git ls-files failed in {}", checkout.display());
    }
    let mut tree = Vec::new();
    let mut bytes = 0usize;
    for path in String::from_utf8_lossy(&out.stdout).lines() {
        if !base_paths.is_empty() && !base_paths.iter().any(|p| path.starts_with(p.as_str())) {
            continue;
        }
        let full = checkout.join(path);
        // Skip anything that is not valid UTF-8 (a stray binary) rather than fail
        // the whole run; a source tree is text and a binary in it is not a gate
        // input.
        let Ok(content) = std::fs::read_to_string(&full) else { continue };
        bytes += content.len();
        tree.push(json!({ "path": path, "content": content }));
    }
    if tree.is_empty() {
        bail!(
            "no tracked files under {:?} in {} — check base_paths",
            base_paths,
            checkout.display()
        );
    }
    // The whole tree travels over wrpc as one message, and NATS refuses one past
    // `max_payload` with a failure that is opaque at this end — so it is caught
    // here, with a message that says what to do.
    //
    // The bound comes from `fleet::max_tree_payload()` rather than a constant
    // here, because the same number configures the server (`fleet.rs` writes it
    // into the nats config it starts with). Hardcoded at one end and configured at
    // the other is how a raised server still gets refused by its own client, which
    // is precisely the bug this replaced: 900_000 stayed put when the ceiling
    // moved to 8 MB.
    let ceiling = comp_reconciler::fleet::max_tree_payload();
    if bytes > ceiling {
        bail!(
            "the base tree is {:.1} MB, over the {:.1} MB a run can ship — scope the goal with \
             base_paths to the crate it touches (a monorepo cannot ship whole)",
            bytes as f64 / 1_048_576.0,
            ceiling as f64 / 1_048_576.0
        );
    }
    Ok(tree)
}

pub(crate) fn head_commit(checkout: &Path) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "HEAD"])
        .output()
        .context("git rev-parse")?;
    if !out.status.success() {
        bail!("git rev-parse HEAD failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The components a run created, as (name, path).
///
/// `components/<name>/...` is the only shape that names a component in this
/// repository — the same rule `plug::tags_for` already uses to decide what a
/// lesson is about, so the two cannot disagree about what a component is.
///
/// Derived from paths rather than announced by the model: a run that SAYS it
/// built a reusable component and a run that actually left one in the tree are
/// different things, and only the second changes what the pool can do.
pub(crate) fn new_capabilities(files: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = files
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f["path"].as_str())
                .filter_map(|p| {
                    let rest = p.strip_prefix("components/")?;
                    let name = rest.split('/').next()?;
                    (!name.is_empty()).then(|| (name.to_string(), format!("components/{name}")))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// Write a candidate's files under `target/goalrun/candidate/`, mirroring paths.
///
/// Returns the directory. The tree it was run against is never touched — the point
/// is to be able to diff, not to have been changed.
pub(crate) fn write_candidate(checkout: &Path, files: &Value) -> std::io::Result<PathBuf> {
    let dir = checkout.join("target/goalrun/candidate");
    // Cleared rather than merged: a stale file from an earlier run sitting beside a
    // fresh one is indistinguishable from the candidate having written it.
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    for f in files.as_array().map(Vec::as_slice).unwrap_or_default() {
        let (Some(path), Some(content)) =
            (f.get("path").and_then(Value::as_str), f.get("content").and_then(Value::as_str))
        else {
            continue;
        };
        // A candidate's paths are checked by the applier before it can land; this is
        // a second look, because writing one to disk is the moment a `../` would
        // escape the directory.
        if path.contains("..") || Path::new(path).is_absolute() {
            continue;
        }
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, content)?;
    }
    Ok(dir)
}

/// Gate commands whose script is not in the shipped tree.
///
/// The third state the critic could not see. A check that fails because its script
/// is missing looks EXACTLY like a check that fails because the work is not done:
/// both are a non-zero exit on the base tree, so the critic says "every check can
/// judge" and the run proceeds to score every branch zero for the operator's
/// reason. `gate.sh` was untracked once and cost a smoke cycle to find; a real run
/// would have cost four branches and a repair round.
///
/// Unlike "did this fail because the tree cannot build", this needs no heuristic on
/// the words in a log. The tree is a list of paths and the command names one, so the
/// question is answered by looking.
///
/// Only arguments that are unambiguously a repo file are considered — a path-shaped
/// argument with a script extension, no URL scheme, not a flag. `cargo test -p x`
/// names no path and is left alone.
pub(crate) fn gates_missing_from_the_tree(goal: &GoalSpec, tree: &[Value]) -> Vec<String> {
    let shipped: std::collections::BTreeSet<&str> =
        tree.iter().filter_map(|f| f.get("path").and_then(Value::as_str)).collect();

    let looks_like_a_repo_script = |arg: &String| -> bool {
        !arg.starts_with('-')
            && arg.contains('/')
            && !arg.contains("://")
            && [".sh", ".py", ".mjs", ".js"].iter().any(|ext| arg.ends_with(ext))
    };

    goal.checks
        .iter()
        .chain(goal.parts.iter().flat_map(|p| p.checks.iter()))
        .flat_map(|c| c.command.iter().filter(|a| looks_like_a_repo_script(a)).map(move |a| (c, a)))
        .filter(|(_, arg)| !shipped.contains(arg.as_str()))
        .map(|(c, arg)| {
            let on_disk = Path::new(arg.as_str()).exists();
            let why = if on_disk {
                "it exists in the checkout but was not shipped — an UNTRACKED file is not in the \
                 base tree, and `base_paths` only ships tracked files. `git add` it"
            } else {
                "no such file in the checkout either — check the path"
            };
            format!("`{}` runs `{}`, which is not in the tree: {}", c.id, arg, why)
        })
        .collect()
}

/// Can this gate judge anything at all?
///
/// A check that already passes on the base tree cannot judge a candidate: one that
/// changes nothing satisfies it. The first real decomposed run on this repository
/// scored 1000 on two candidates that had deleted their own component exports,
/// because `cargo component check` passes on a crate implementing none of its
/// world (goal 07). This is the cheapest possible place to find that out — before
/// a generation buys the wrong answer.
///
/// `Ok(false)` means the run should stop, having spent nothing. A critic that
/// cannot RUN is reported and ignored: a guard that fails must not stop work a
/// person asked for.
pub(crate) fn gate_can_judge(
    goal: &GoalSpec,
    checks: &[Value],
    gate: &Gate,
    base_commit: &str,
    tree: &[Value],
    timeout: u64,
) -> bool {
    let missing = gates_missing_from_the_tree(goal, tree);
    if !missing.is_empty() {
        println!("\nREFUSED — a gate that cannot run cannot judge:\n");
        for m in &missing {
            println!("  · {m}");
        }
        println!("\nNothing was spent. Every branch would have scored zero for this reason.");
        return false;
    }

    let excused: Vec<String> = goal
        .checks
        .iter()
        .chain(goal.parts.iter().flat_map(|p| p.checks.iter()))
        .filter(|c| c.may_pass_base)
        .map(|c| c.id.clone())
        .collect();
    let mut every_check: Vec<Value> = checks.to_vec();
    for p in &goal.parts {
        every_check.extend(p.checks.iter().map(|c| {
            json!({ "id": c.id, "required": c.required, "weight": c.weight, "command": c.command, "needs": c.needs })
        }));
    }
    match compose::criticise(
        &gate.url(),
        gate.token().as_deref(),
        base_commit,
        &json!(tree),
        &json!(every_check),
        &excused,
        Duration::from_secs(timeout),
    ) {
        Ok(base) => {
            let vacuous = &base.vacuous;
            for v in vacuous.iter().filter(|v| v.excused) {
                println!("gate: `{}` passes on the base, and says it is meant to", v.id);
            }
            let refusals = compose::refusal(vacuous);
            if !refusals.is_empty() {
                println!("\nREFUSED — this gate cannot judge anything:\n");
                for r in &refusals {
                    println!("  · {r}");
                }
                println!(
                    "\nNothing was spent. A gate that passes on the code as it stands \n\
                     accepts a candidate that changes nothing."
                );
                return false;
            }
            println!("gate: every check fails on the base tree, so every check can judge");
            // WHY each one failed, because "it failed" is two states wearing one face:
            // work not done yet (a gate that can judge) and a tree that cannot build (a
            // gate that will fail every branch identically, for the operator's reason).
            // Printed rather than guessed at: a heuristic on the word "compile" would
            // refuse legitimate goals, and `tools/goal-rehearse.sh` is where this is
            // actually caught before anything is spent.
            for r in &base.reasons {
                let first = r.lines().next().unwrap_or(r).trim();
                println!("  · {}", &first[..first.len().min(160)]);
            }
            true
        }
        // Reported, not fatal: the critic is a guard, and a guard that cannot run
        // must not stop a run a person asked for.
        Err(e) => {
            println!("gate: could not be criticised ({e}) — running anyway");
            true
        }
    }
}

/// Prove the whole rig without spending anything.
///
/// Both apps serving already proves a lot: an app whose secret cannot be granted,
/// or whose egress is malformed, fails to START and never serves (`select.rs` saw
/// exactly this). So reaching here means links resolve, egress allow-lists parsed,
/// and every secret was granted.
///
/// A `max_attempts: 0` run is refused by the driver BEFORE any model call, so the
/// last round trip proves probe→driver for free.
#[allow(clippy::too_many_arguments)]
pub(crate) fn smoke(
    args: &Args,
    goal: &GoalSpec,
    port: u16,
    context: &[Value],
    checks: &[Value],
    base_commit: &str,
    allow: &[&str],
) -> Result<()> {
    // Both apps serving already proves a lot: an app whose secret cannot be
    // granted, or whose egress is malformed, fails to START and never serves
    // (select.rs saw exactly this). So reaching here means links resolve,
    // egress allow-lists parsed, and BOTH secrets were granted.
    //
    // A max_attempts:0 run is refused by the driver BEFORE any model call, so
    // this last round-trip proves probe→driver without spending anything.
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build()?;
    let probe = http
        .post(format!("http://127.0.0.1:{port}/run"))
        .header("host", "goalrun.acme.test")
        .body(
            json!({
                "text": goal.text, "writable": goal.writable, "context": context,
                "previous": [], "checks": checks, "base_commit": base_commit,
                // Empty, like every other plan: the runner holds the tree. This
                // one runs `max_attempts: 0` and never reaches the gate at all —
                // it exists to prove probe -> driver — so carrying a repository
                // through it was pure postage.
                "base_tree": [], "max_attempts": 0, "seed": 1,
            })
            .to_string(),
        )
        .send()?;
    let body: Value =
        serde_json::from_str(&probe.text().unwrap_or_default()).unwrap_or(Value::Null);
    println!("\nSMOKE OK:");
    println!("  · both graphs started and serve → links, egress and secret GRANTS are correct");
    println!("  · driver reachable → {body}");

    // A decomposed goal brings up two more apps and a database, and every one
    // of them can be proved for FREE: an app whose secret cannot be granted or
    // whose egress is malformed never serves, publishing the contract exercises
    // the registry through the graph to a real SurrealDB, and `describe` asks
    // the provider what it is without asking it to think.
    if !goal.parts.is_empty() {
        wait_serving(port, "goalcontract.acme.test", Duration::from_secs(180))?;
        wait_serving(port, "goalanswer.acme.test", Duration::from_secs(180))?;
        println!("  · the contract registry and the answer door serve");

        let registry = Registry {
            url: format!("http://127.0.0.1:{port}"),
            host: "goalcontract.acme.test".into(),
            timeout: Duration::from_secs(60),
        };
        let contract_path = goal.contract.clone().unwrap_or_default();
        let contract = std::fs::read_to_string(args.checkout.join(&contract_path))
            .with_context(|| format!("reading the contract at {contract_path}"))?;
        match registry.publish(&contract) {
            Ok(v) => println!(
                "  · contract v{v} published from {contract_path} → registry → graph → \
                 SurrealDB, and the database's secret was granted"
            ),
            // A second smoke run against the same database finds the contract
            // already there, which proves the same chain and is not a failure.
            Err(e) if e.contains("already published") => match registry.current() {
                Ok(c) => println!(
                    "  · contract v{} already in the registry, and readable → the whole \
                     chain to SurrealDB works",
                    c.number
                ),
                Err(e) => bail!("the registry has a contract it cannot read back: {e}"),
            },
            Err(e) => bail!("the contract registry is not usable: {e}"),
        }
        let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build()?;
        let describe = http
            .get(format!("http://127.0.0.1:{port}/describe"))
            .header("host", "goalanswer.acme.test")
            .send()?;
        let d: Value =
            serde_json::from_str(&describe.text().unwrap_or_default()).unwrap_or(Value::Null);
        println!("  · the answering model is reachable and says it is → {d}");
        println!(
            "  · parts: {}",
            goal.parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
        );
    }

    println!("\nWhat smoke does NOT check (needs a real call, costs money):");
    println!("  · that the Anthropic key VALUE is accepted");
    println!("  · that the GitHub token VALUE can open a PR");
    println!("  · that `{}` actually runs under the gate", allow.join(" "));
    if !goal.parts.is_empty() {
        println!("  · that the parts negotiate — the first request costs one small call");
    }
    println!("\nRun for real by dropping --smoke.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CheckSpec;

    /// A gate whose script is not in the shipped tree must be refused BEFORE a
    /// branch is spent, because a missing script and unfinished work are the same
    /// exit code — and the run would score every candidate zero for the operator's
    /// reason. This is the case `gate.sh` being untracked actually produced.
    #[test]
    fn a_gate_script_missing_from_the_tree_is_refused() {
        let goal = |cmd: Vec<&str>| GoalSpec {
            text: "t".into(),
            writable: vec![],
            title: None,
            base_paths: vec![],
            workspace_manifest: None,
            keep_members: vec![],
            component: None,
            context: vec![],
            checks: vec![CheckSpec {
                id: "spec".into(),
                may_pass_base: false,
                required: true,
                weight: 1,
                command: cmd.into_iter().map(String::from).collect(),
                needs: vec![],
            }],
            contract: None,
            parts: vec![],
        };
        let tree = vec![json!({ "path": "components/x/shipped.sh", "content": "" })];

        assert!(
            gates_missing_from_the_tree(&goal(vec!["bash", "components/x/shipped.sh"]), &tree)
                .is_empty(),
            "a shipped script is fine"
        );

        let missing =
            gates_missing_from_the_tree(&goal(vec!["bash", "components/x/absent.sh"]), &tree);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("absent.sh"), "{}", missing[0]);
        assert!(
            missing[0].contains("no such file"),
            "not on disk either, so say so: {}",
            missing[0]
        );

        // The discriminator has to leave ordinary commands alone. None of these
        // names a repo script, and refusing any of them would break real goals.
        for cmd in [
            vec!["cargo", "test", "-p", "card-identify"],
            vec!["curl", "-sf", "http://127.0.0.1:8080/health"],
            vec!["just", "e2e-binder"],
            vec!["bash", "-c", "cd components && cargo test"],
        ] {
            assert!(
                gates_missing_from_the_tree(&goal(cmd.clone()), &tree).is_empty(),
                "{cmd:?} names no repo script and must not be refused"
            );
        }
    }

    /// A component is `components/<name>/…` and nothing else.
    ///
    /// The rule matters because it decides what the pool believes it gained: a
    /// path outside `components/` is app code, and counting it would report a
    /// capability that no future run can reuse.
    #[test]
    fn only_components_count_as_a_new_capability() {
        let files = serde_json::json!([
            { "path": "components/csv-codec/src/lib.rs", "content": "" },
            { "path": "components/csv-codec/Cargo.toml", "content": "" },
            { "path": "apps/vet/src/main.rs", "content": "" },
            { "path": "README.md", "content": "" },
            { "path": "components/paginate/wit/p.wit", "content": "" },
        ]);
        assert_eq!(
            new_capabilities(&files),
            vec![
                ("csv-codec".to_string(), "components/csv-codec".to_string()),
                ("paginate".to_string(), "components/paginate".to_string()),
            ],
            "one entry per COMPONENT, not per file, and nothing outside components/"
        );
    }

    /// A run that wrote no component gained the pool nothing, and must say so
    /// rather than reporting an empty-named capability.
    #[test]
    fn a_run_that_built_no_component_adds_no_capability() {
        let files = serde_json::json!([
            { "path": "apps/vet/src/main.rs", "content": "" },
            { "path": "components/", "content": "" },
        ]);
        assert!(new_capabilities(&files).is_empty());
    }
}
