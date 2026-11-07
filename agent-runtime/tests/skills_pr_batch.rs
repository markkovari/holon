//! Verify graph-native skills, autonomous agent PR proposing with graph provenance,
//! and free-tier batch queue model completion ($0.00 spend).

use agent_runtime::agent::Cause;
use agent_runtime::model::{LocalModel, Msg};
use agent_runtime::runtime::{Config, Runtime};
use agent_runtime::spec::{AgentSpec, Capability, ModelSpec};
use agent_wire::Step;
use std::sync::Arc;
use std::sync::Mutex;

fn runtime(tag: &str) -> Arc<Runtime> {
    let dir =
        tempfile::Builder::new().prefix(&format!("ar-skills-{tag}-")).tempdir().unwrap().keep();
    Runtime::new(Config::new(dir)).unwrap()
}

fn agent_with_replies(name: &str, replies: &[&str]) -> AgentSpec {
    let mut s = AgentSpec::new(name, "a test agent");
    s.model = ModelSpec::Mock { replies: replies.iter().map(|r| r.to_string()).collect() };
    s
}

#[test]
fn test_skill_query_tool_resolves_graph_subskills_and_wit_interfaces() {
    let rt = runtime("skill-query");

    let mut a = agent_with_replies(
        "architect",
        &[
            r#"{"tool": "skill_query", "args": {"skill_id": "agent:deliver_feature"}}"#,
            "I reviewed the skill DAG and resolved its prerequisite WIT interfaces.",
        ],
    );
    a.capabilities.push(Capability::named("skill_query"));
    rt.create_agent(a).unwrap();

    let rec = rt
        .run_agent(
            "architect",
            "http",
            "how do we deliver this feature?",
            &Cause::default(),
            "",
            true,
        )
        .unwrap();

    let tool_step = rec
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, error, .. } if name == "skill_query" => {
                Some((result.clone(), *error))
            }
            _ => None,
        })
        .expect("skill_query tool step found");

    assert!(!tool_step.1);
    assert!(tool_step.0.contains("git:forge/repo@0.1.0"));
    assert!(tool_step.0.contains("code:generate_batch"));
    assert!(tool_step.0.contains("rust:compile_check"));
}

#[test]
fn test_skill_activate_wit_contract_first_scaffold() {
    let rt = runtime("activate-wit");

    let mut a = agent_with_replies(
        "scaffolder",
        &[
            r#"{"tool": "skill_activate", "args": {"skill_id": "wit:contract_first_scaffold"}}"#,
            "I have loaded the WIT contract-first scaffolding plan.",
        ],
    );
    a.capabilities.push(Capability::named("skill_activate"));
    rt.create_agent(a).unwrap();

    let rec = rt
        .run_agent("scaffolder", "http", "prepare scaffolding", &Cause::default(), "", true)
        .unwrap();

    let tool_step = rec
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, error, .. } if name == "skill_activate" => {
                Some((result.clone(), *error))
            }
            _ => None,
        })
        .expect("skill_activate tool step found");

    assert!(!tool_step.1);
    assert!(tool_step.0.contains("wit:contract_first_scaffold"));
    assert!(tool_step.0.contains("wit:draft_interface"));
    assert!(tool_step.0.contains("bindings:generate_xtask"));
    assert!(tool_step.0.contains("wit:reflect@0.1.0"));
}

#[test]
fn test_skill_activate_bug_repro_and_repair() {
    let rt = runtime("activate-bug");

    let mut a = agent_with_replies(
        "debugger",
        &[
            r#"{"tool": "skill_activate", "args": {"skill_id": "bug:autonomous_repro_and_repair"}}"#,
            "I have activated the autonomous bug reproduction and gate repair plan.",
        ],
    );
    a.capabilities.push(Capability::named("skill_activate"));
    rt.create_agent(a).unwrap();

    let rec =
        rt.run_agent("debugger", "http", "fix issue #102", &Cause::default(), "", true).unwrap();

    let tool_step = rec
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, error, .. } if name == "skill_activate" => {
                Some((result.clone(), *error))
            }
            _ => None,
        })
        .expect("skill_activate tool step found");

    assert!(!tool_step.1);
    assert!(tool_step.0.contains("bug:autonomous_repro_and_repair"));
    assert!(tool_step.0.contains("test:author_failing_gate"));
    assert!(tool_step.0.contains("gate:prove_initial_failure"));
    assert!(tool_step.0.contains("graph:fitness/evaluator"));
}

#[test]
fn test_agent_with_configured_skills_loads_dags_into_system_prompt() {
    let rt = runtime("spec-skills");

    let mut a = agent_with_replies("lead_eng", &["I am ready with both skills loaded."]);
    a.skills = vec![
        "wit:contract_first_scaffold".to_string(),
        "bug:autonomous_repro_and_repair".to_string(),
    ];
    let prompt = agent_runtime::agent::system_prompt(rt.as_ref(), &a);
    assert!(prompt.contains("Active Graph-Native Skills:"));
    assert!(prompt.contains("[wit:contract_first_scaffold]"));
    assert!(prompt.contains("wit:draft_interface"));
    assert!(prompt.contains("bindings:generate_xtask"));
    assert!(prompt.contains("[bug:autonomous_repro_and_repair]"));
    assert!(prompt.contains("test:author_failing_gate"));
    assert!(prompt.contains("gate:verify_green"));
}

#[test]
fn propose_pr_pushes_a_branch_opens_a_real_pr_and_records_it() {
    let (rt, dir) = runtime_with_forge("propose-pr");

    let mut a = agent_with_replies(
        "coder",
        &[
            r#"{"tool": "propose_pr", "args": {"branch": "feat/skill-graph-pr", "title": "feat(core): add skill graph and forge pr", "body": "Resolves issue #42", "changes": [{"path": "docs/skills.md", "content": "graph\n"}]}}"#,
            "PR opened successfully with provenance recorded.",
        ],
    );
    a.capabilities.push(Capability::named("propose_pr"));
    a.auto_approve.push("propose_pr".into());
    rt.create_agent(a).unwrap();

    let rec = rt
        .run_agent("coder", "http", "open a PR for this work", &Cause::default(), "", true)
        .unwrap();

    let tool_step = rec
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, error, .. } if name == "propose_pr" => {
                Some((result.clone(), *error))
            }
            _ => None,
        })
        .expect("propose_pr tool step found");

    assert!(!tool_step.1, "{}", tool_step.0);
    let opened: serde_json::Value = serde_json::from_str(&tool_step.0).unwrap();
    assert_eq!(
        opened["url"], "https://github.com/o/r/pull/7",
        "what gh answered, not a made-up number"
    );
    assert_eq!(opened["number"], 7);
    assert_eq!(
        on_origin(&dir, "feat/skill-graph-pr", "docs/skills.md"),
        "graph\n",
        "the branch is pushed"
    );
    let commit = opened["commit"].as_str().unwrap();
    assert_eq!(on_origin(&dir, "feat/skill-graph-pr", "README.md"), "base\n", "off main");
    assert_eq!(commit.len(), 40);
    let gh_args = std::fs::read_to_string(dir.join("gh.args")).unwrap();
    assert!(
        gh_args.contains("--head feat/skill-graph-pr") && gh_args.contains("--base main"),
        "{gh_args}"
    );

    // Verify PR provenance was recorded in knowledge store
    let rows = rt.kv().list("pr_provenance", "pr:").unwrap();
    assert!(!rows.is_empty());
    assert!(rows
        .iter()
        .any(|(k, entry)| k.starts_with("pr:") && entry.value.contains("feat/skill-graph-pr")));
}

#[test]
fn the_free_queue_unreachable_is_an_error_never_a_made_up_reply() {
    let spec = ModelSpec::BatchQueue {
        queue_url: "http://127.0.0.1:1".into(),
        model: "gemini-2.5-flash-lite".into(),
    };
    let local = LocalModel::default();
    let mock_cursor = Mutex::new(0);
    let msgs = vec![Msg::user("write a rust function for adding two numbers")];

    let err =
        agent_runtime::model::complete(&spec, &local, "System coder prompt", &msgs, &mock_cursor)
            .unwrap_err();
    assert!(err.contains("free queue") && err.contains("nats://127.0.0.1:1"), "{err}");
    assert_eq!(spec.label(), "batch:gemini-2.5-flash-lite");
}

#[test]
fn test_skill_curator_agent_audits_registry_and_proposes_refactor_pr() {
    let (rt, dir) = runtime_with_forge("curator");

    let mut a = agent_with_replies(
        "curator-agent",
        &[
            r#"{"tool": "skill_audit", "args": {}}"#,
            r#"{"tool": "propose_pr", "args": {"branch": "refactor/dedup-skills", "title": "refactor(skills): deduplicate registry and alias legacy workflows", "body": "Periodic curator audit detected near-duplicate workflows; consolidated definitions.", "changes": [{"path": "skills/aliases.md", "content": "legacy -> deliver_feature\n"}]}}"#,
            "Curator audit completed and PR opened.",
        ],
    );
    a.capabilities.push(Capability::named("skill_audit"));
    a.capabilities.push(Capability::named("propose_pr"));
    a.auto_approve.push("skill_audit".into());
    a.auto_approve.push("propose_pr".into());
    a.skills.push("skill:curate_registry".into());
    rt.create_agent(a).unwrap();

    let rec = rt
        .run_agent(
            "curator-agent",
            "cron",
            "run periodic hygiene review",
            &Cause::default(),
            "",
            true,
        )
        .unwrap();

    let audit_step = rec
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, error, .. } if name == "skill_audit" => {
                Some((result.clone(), *error))
            }
            _ => None,
        })
        .expect("skill_audit tool step found");

    assert!(!audit_step.1);
    assert!(audit_step.0.contains("total_skills"));

    let pr_step = rec
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, error, .. } if name == "propose_pr" => {
                Some((result.clone(), *error))
            }
            _ => None,
        })
        .expect("propose_pr tool step found");

    assert!(!pr_step.1, "{}", pr_step.0);
    assert!(pr_step.0.contains("refactor/dedup-skills"));
    assert_eq!(
        on_origin(&dir, "refactor/dedup-skills", "skills/aliases.md"),
        "legacy -> deliver_feature\n"
    );
}

/// A runtime whose `propose_pr` pushes to a local bare `origin` and whose `gh` is a stub that
/// answers PR 7's URL: what a PR test checks is then what really happened, with no GitHub.
fn runtime_with_forge(tag: &str) -> (Arc<Runtime>, std::path::PathBuf) {
    let dir = tempfile::Builder::new().prefix(&format!("ar-pr-{tag}-")).tempdir().unwrap().keep();
    let sh = |args: &[&str], at: &std::path::Path| {
        let out = std::process::Command::new("git").args(args).current_dir(at).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    let origin = dir.join("origin.git");
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    sh(&["init", "--quiet", "--bare", "-b", "main", origin.to_str().unwrap()], &dir);
    sh(&["init", "--quiet", "-b", "main"], &repo);
    sh(&["config", "user.email", "t@t"], &repo);
    sh(&["config", "user.name", "t"], &repo);
    std::fs::write(repo.join("README.md"), "base\n").unwrap();
    sh(&["add", "."], &repo);
    sh(&["-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "base"], &repo);
    sh(&["remote", "add", "origin", origin.to_str().unwrap()], &repo);
    sh(&["push", "--quiet", "origin", "main"], &repo);
    let gh = dir.join("gh");
    std::fs::write(&gh, "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/gh.args\"\necho https://github.com/o/r/pull/7\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = Config::new(dir.join("state"));
    cfg.pr = Some(agent_runtime::pr::Forge { repo, gh });
    (Runtime::new(cfg).unwrap(), dir)
}

fn on_origin(dir: &std::path::Path, branch: &str, path: &str) -> String {
    let out = std::process::Command::new("git")
        .args(["--git-dir", dir.join("origin.git").to_str().unwrap(), "show"])
        .arg(format!("{branch}:{path}"))
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn without_a_forge_propose_pr_refuses_and_never_invents_a_pr() {
    let rt = runtime("no-forge");
    let mut a = agent_with_replies(
        "coder",
        &[
            r#"{"tool": "propose_pr", "args": {"branch": "feat/x", "title": "x", "changes": [{"path": "a", "content": "b"}]}}"#,
            "done",
        ],
    );
    a.capabilities.push(Capability::named("propose_pr"));
    a.auto_approve.push("propose_pr".into());
    rt.create_agent(a).unwrap();
    let rec = rt.run_agent("coder", "http", "open a PR", &Cause::default(), "", true).unwrap();
    let (result, error) = rec
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, error, .. } if name == "propose_pr" => {
                Some((result.clone(), *error))
            }
            _ => None,
        })
        .unwrap();
    assert!(error && result.contains("--pr-repo"), "{result}");
    assert!(rt.kv().list("pr_provenance", "pr:").unwrap().is_empty(), "no provenance for no PR");
}

#[test]
fn a_built_tool_is_proposed_as_its_whole_component() {
    let (rt, dir) = runtime_with_forge("tool-pr");
    // What tool_build leaves: the template's copy with the agent's tool.mbt, and its build output.
    let work = dir.join("state/forge/helper-prime_count");
    for (p, c) in [
        ("tool/tool.mbt", "pub fn call() {}"),
        ("moon.mod", "{}"),
        ("wit/world.wit", "w"),
        ("_build/x.wasm", "x"),
        ("embedded.wasm", "embedded"),
        ("tool.wasm", "component"),
        ("text/text.mbt", "legacy"),
        ("tool.mbt", "stray root"),
    ] {
        std::fs::create_dir_all(work.join(p).parent().unwrap()).unwrap();
        std::fs::write(work.join(p), c).unwrap();
    }
    let mut a = agent_with_replies(
        "helper",
        &[
            r#"{"tool": "propose_pr", "args": {"branch": "tool/prime-count", "title": "feat(tools): prime_count", "tool": "prime_count", "changes": [{"path": "components/prime-count-mbt/tool.mbt", "content": "hallucinated"}]}}"#,
            "proposed",
        ],
    );
    a.capabilities.push(Capability::named("propose_pr"));
    a.auto_approve.push("propose_pr".into());
    rt.create_agent(a).unwrap();
    let rec = rt.run_agent("helper", "http", "propose it", &Cause::default(), "", true).unwrap();
    let ok = rec
        .steps
        .iter()
        .any(|s| matches!(s, Step::Tool { name, error: false, .. } if name == "propose_pr"));
    assert!(ok, "{:?}", rec.steps);
    let at = |p: &str| on_origin(&dir, "tool/prime-count", p);
    assert_eq!(at("components/prime-count-mbt/tool/tool.mbt"), "pub fn call() {}");
    assert_eq!(at("components/prime-count-mbt/wit/world.wit"), "w");
    assert_eq!(at("components/prime-count-mbt/_build/x.wasm"), "", "build output is not proposed");
    assert_eq!(
        at("components/prime-count-mbt/embedded.wasm"),
        "",
        "wasm artifacts are not proposed"
    );
    assert_eq!(at("components/prime-count-mbt/tool.wasm"), "", "wasm artifacts are not proposed");
    assert_eq!(at("components/prime-count-mbt/text/text.mbt"), "", "text package is not proposed");
    assert_eq!(
        at("components/prime-count-mbt/tool.mbt"),
        "",
        "stray root tool.mbt is not proposed"
    );
}
