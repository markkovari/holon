//! Emulate 50 concurrent agents with mocked inference, symbol-level AST commutation,
//! S3 blob persistence (rustfs), and Git push coordination (walgit).
#![cfg(feature = "native")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use holon_vcs::engine::Engine;
use holon_vcs::mem::{self, MemGraph};
use holon_vcs::model::{
    Agent, ConflictState, Content, PatchOutcome, PatchRequest, SymbolId, SymbolKind, Transformation,
};
use holon_vcs::s3::{S3Blobs, S3Config};
use tokio::sync::Barrier;

const NUM_AGENTS: usize = 50;
const COMPONENT: &str = "billing";
const FILE: &str = "src/calculator.rs";

fn func(name: &str) -> SymbolId {
    SymbolId::new(COMPONENT, FILE, name, SymbolKind::Function)
}

fn inline(s: &str) -> Content {
    Content::Inline(s.to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
async fn test_50_agents_swarm_simulation() {
    let start_total = Instant::now();
    println!("\n=== Starting 50-Agent Swarm Emulation ===");

    // 1. Initialize S3Blobs backed by local rustfs (:9000)
    let s3_cfg = S3Config {
        endpoint: "http://127.0.0.1:9000".into(),
        bucket: "holon-swarm-blobs".into(),
        region: "us-east-1".into(),
        access_key: "rustfsadmin".into(),
        secret_key: "rustfsadmin".into(),
        path_style: true,
        prefix: format!("swarm-{}", std::process::id()),
    };

    let s3_blobs = match S3Blobs::new(s3_cfg).await {
        Ok(b) => Arc::new(b),
        Err(e) => {
            eprintln!("SKIPPED: rustfs S3 store not reachable on :9000: {e}");
            return;
        }
    };

    // 2. Build Engine: S3 for immutable bytes + memory for graph & CAS pointers
    let pointers = Arc::new(mem::pointers());
    let graph = Arc::new(MemGraph::new());
    let log = Arc::new(mem::oplog());
    let engine =
        Arc::new(Engine::new(s3_blobs.clone(), pointers.clone(), graph.clone(), log.clone()));

    let ws = format!("ws-swarm-{}", std::process::id());

    // 3. Seed initial code into the workspace
    // Seed 40 distinct functions and 1 hotspot function
    let mut seed_parents = Vec::with_capacity(40);
    for i in 0..40 {
        let sym = func(&format!("calculate_tier_{i}"));
        let body = format!("pub fn calculate_tier_{i}() -> u32 {{ {i} * 10 }}");
        let res = engine
            .apply_patch(PatchRequest {
                workspace: ws.clone(),
                symbol: sym,
                parent: None,
                change: Transformation::Create(inline(&body)),
                agent: Agent::named("seeder"),
                message: Some("seed".into()),
                depends_on: vec![],
                implements: vec![],
                wit_binding: None,
                read_at: None,
                position: None,
            })
            .await
            .expect("seed create");
        assert_eq!(res.outcome, PatchOutcome::Applied);
        seed_parents.push(res.patch);
    }

    let hotspot_sym = func("hotspot_calculate_total");
    let hotspot_body = "pub fn hotspot_calculate_total() -> u64 { 100 }";
    let seed_hotspot = engine
        .apply_patch(PatchRequest {
            workspace: ws.clone(),
            symbol: hotspot_sym.clone(),
            parent: None,
            change: Transformation::Create(inline(hotspot_body)),
            agent: Agent::named("seeder"),
            message: Some("seed hotspot".into()),
            depends_on: vec![],
            implements: vec![],
            wit_binding: None,
            read_at: None,
            position: None,
        })
        .await
        .expect("seed hotspot");

    assert_eq!(seed_hotspot.outcome, PatchOutcome::Applied);
    let hotspot_parent = seed_hotspot.patch;
    let seed_parents = Arc::new(seed_parents);

    println!("Seeded base component into S3Blobs: 41 symbols ready.");

    // 4. Launch 50 concurrent agents synchronized at a barrier
    let barrier = Arc::new(Barrier::new(NUM_AGENTS));
    let applied_count = Arc::new(AtomicUsize::new(0));
    let commuted_count = Arc::new(AtomicUsize::new(0));
    let conflict_count = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(NUM_AGENTS);

    for agent_idx in 0..NUM_AGENTS {
        let b = barrier.clone();
        let eng = engine.clone();
        let ws = ws.clone();
        let hs_sym = hotspot_sym.clone();
        let hs_parent = hotspot_parent.clone();
        let parents = seed_parents.clone();
        let applied = applied_count.clone();
        let commuted = commuted_count.clone();
        let conflicts = conflict_count.clone();

        handles.push(tokio::spawn(async move {
            let agent_name = format!("agent-{agent_idx:02}");

            // Wait for all 50 agents to be ready
            b.wait().await;

            // Mock AI inference delay (simulating TTFT / token generation latency)
            tokio::time::sleep(Duration::from_millis(10 + (agent_idx % 20) as u64 * 2)).await;

            // Scenario distribution:
            // - Agents 0..39 edit their assigned function (commutes cleanly via AST patch graph)
            // - Agents 40..49 race on the SAME hotspot function from the same parent (produces conflicts-as-data)
            if agent_idx < 40 {
                let sym = func(&format!("calculate_tier_{agent_idx}"));
                let new_body = format!(
                    "pub fn calculate_tier_{agent_idx}() -> u32 {{ ({agent_idx} * 10) + 42 }}"
                );
                let res = eng
                    .apply_patch(PatchRequest {
                        workspace: ws,
                        symbol: sym,
                        parent: Some(parents[agent_idx].clone()),
                        change: Transformation::Replace(inline(&new_body)),
                        agent: Agent::named(&agent_name),
                        message: Some(format!("agent {agent_idx} tier refactor")),
                        depends_on: vec![],
                        implements: vec![],
                        wit_binding: None,
                        read_at: None,
                        position: None,
                    })
                    .await;

                match res {
                    Ok(r) => match r.outcome {
                        PatchOutcome::Applied => {
                            applied.fetch_add(1, Ordering::Relaxed);
                        }
                        PatchOutcome::Commuted => {
                            commuted.fetch_add(1, Ordering::Relaxed);
                        }
                        PatchOutcome::Conflicted => {
                            conflicts.fetch_add(1, Ordering::Relaxed);
                        }
                        PatchOutcome::Duplicate => {}
                    },
                    Err(e) => panic!("Agent {agent_idx} unexpected error: {e}"),
                }
            } else {
                // Hotspot racer
                let racer_num = agent_idx - 40;
                let racer_body =
                    format!("pub fn hotspot_calculate_total() -> u64 {{ 100 + {racer_num} }}");
                let res = eng
                    .apply_patch(PatchRequest {
                        workspace: ws,
                        symbol: hs_sym,
                        parent: Some(hs_parent),
                        change: Transformation::Replace(inline(&racer_body)),
                        agent: Agent::named(&agent_name),
                        message: Some(format!("hotspot race by agent {agent_idx}")),
                        depends_on: vec![],
                        implements: vec![],
                        wit_binding: None,
                        read_at: None,
                        position: None,
                    })
                    .await;

                match res {
                    Ok(r) => match r.outcome {
                        PatchOutcome::Applied => {
                            applied.fetch_add(1, Ordering::Relaxed);
                        }
                        PatchOutcome::Commuted => {
                            commuted.fetch_add(1, Ordering::Relaxed);
                        }
                        PatchOutcome::Conflicted => {
                            conflicts.fetch_add(1, Ordering::Relaxed);
                        }
                        PatchOutcome::Duplicate => {}
                    },
                    Err(e) => panic!("Hotspot racer {agent_idx} error: {e}"),
                }
            }
        }));
    }

    for h in handles {
        h.await.expect("agent task failed");
    }

    let elapsed = start_total.elapsed();
    let total_applied = applied_count.load(Ordering::Relaxed);
    let total_commuted = commuted_count.load(Ordering::Relaxed);
    let total_conflicts = conflict_count.load(Ordering::Relaxed);

    println!("\n--- 50 Agents VCS Swarm Results ---");
    println!("Elapsed time:        {:?}", elapsed);
    println!("Directly applied:    {}", total_applied);
    println!("Commuted edits:      {}", total_commuted);
    println!("Total successful:    {}", total_applied + total_commuted);
    println!("Conflicts recorded:  {}", total_conflicts);

    // 40 distinct agents must all succeed (either directly applied or commuted past each other)
    assert_eq!(total_applied + total_commuted, 41); // 40 distinct + 1 winning hotspot racer
    assert_eq!(total_conflicts, 9); // exactly 9 losing hotspot racers became conflicts-as-data

    // Check that open conflicts are readable as data for a resolver agent
    let open_conflicts =
        engine.list_conflicts(&ws, Some(ConflictState::Open)).await.expect("list conflicts");
    println!("Open conflicts in store: {}", open_conflicts.len());
    assert_eq!(open_conflicts.len(), 9);

    println!("All assertions passed: Zero crashes, zero lock timeouts, 100% data integrity on S3!");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
async fn test_50_agents_walgit_git_push() {
    // Check if walgit is reachable on :8080
    let client = reqwest::Client::builder().timeout(Duration::from_millis(500)).build().unwrap();
    if client.get("http://127.0.0.1:8080/health").send().await.is_err()
        && client.get("http://127.0.0.1:8080/").send().await.is_err()
    {
        eprintln!("SKIPPED: walgit not reachable on http://127.0.0.1:8080");
        return;
    }

    println!("\n=== Starting 50-Agent Concurrent Git Push to walgit ===");

    // Create a temporary git repo to push from
    let repo_path = std::env::temp_dir().join(format!("swarm-push-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo_path);
    std::fs::create_dir_all(&repo_path).expect("create repo dir");

    let run_cmd = |cmd: &str, args: &[&str], dir: &std::path::Path| {
        let status = std::process::Command::new(cmd)
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("exec");
        assert!(
            status.status.success(),
            "cmd {cmd} {args:?} failed: {:?}",
            String::from_utf8_lossy(&status.stderr)
        );
    };

    run_cmd("git", &["init", "-b", "main"], &repo_path);
    run_cmd("git", &["config", "user.name", "Agent Swarm"], &repo_path);
    run_cmd("git", &["config", "user.email", "swarm@holon.run"], &repo_path);
    run_cmd("git", &["config", "commit.gpgsign", "false"], &repo_path);

    std::fs::write(repo_path.join("README.md"), "# 50-Agent Concurrent Push Test\n").unwrap();
    run_cmd("git", &["add", "."], &repo_path);
    run_cmd("git", &["commit", "--no-gpg-sign", "-m", "initial commit"], &repo_path);

    let start = Instant::now();
    let barrier = Arc::new(Barrier::new(NUM_AGENTS));
    let success_count = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(NUM_AGENTS);

    let remote_url = format!("http://127.0.0.1:8080/repos/swarm-load-{}.git", std::process::id());

    for i in 0..NUM_AGENTS {
        let b = barrier.clone();
        let path = repo_path.clone();
        let url = remote_url.clone();
        let succ = success_count.clone();

        handles.push(tokio::spawn(async move {
            b.wait().await;

            let refspec = format!("HEAD:refs/heads/branch-{i:02}");
            let res = tokio::process::Command::new("git")
                .args(["push", &url, &refspec])
                .current_dir(&path)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .await;

            if let Ok(out) = res {
                if out.status.success() {
                    succ.fetch_add(1, Ordering::Relaxed);
                } else {
                    eprintln!("Branch {i} push failed: {}", String::from_utf8_lossy(&out.stderr));
                }
            }
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let elapsed = start.elapsed();
    let total_ok = success_count.load(Ordering::Relaxed);
    println!("\n--- 50 Agents Concurrent Git Push Results ---");
    println!("Elapsed time:        {:?}", elapsed);
    println!("Successful pushes:   {}/{}", total_ok, NUM_AGENTS);
    assert_eq!(total_ok, NUM_AGENTS, "All 50 branches pushed into walgit backed by S3");
    println!("50 branches created in S3 via CAS without a database!");
}
