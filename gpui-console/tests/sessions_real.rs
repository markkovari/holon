//! Runs here too because reqwest's features unify per workspace, so this crate's client can differ.
//! Needs `cargo build -p comp-agentd`.

use std::time::{Duration, Instant};

use agent_runtime::agent::Cause;
use agent_runtime::session::{AgentdConfig, Phase};
use agent_runtime::{AgentSpec, Capability, Config, ModelSpec, Runtime};

const TOKEN: &str = "3f9a1c7e5b2d4a60918e7c3b5d1f0a42";

struct Daemon(std::process::Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore]
fn a_session_is_watched_through_this_crates_http_client() {
    if std::env::var("SESS_FAKE_HOME").is_ok() {
        let h = tempfile::Builder::new().prefix("gc-home-").tempdir().unwrap().keep();
        std::env::set_var("HOME", h);
    }
    let state = tempfile::Builder::new().prefix("gc-sess-").tempdir().unwrap().keep();
    let root = {
        let w = state.join("workspaces");
        std::fs::create_dir_all(&w).unwrap();
        std::fs::canonicalize(w).unwrap()
    };
    let bin = {
        let b1 = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/debug/holon-agentd");
        let b2 = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/debug/comp-agentd");
        if b1.exists() { b1 } else { b2 }
    };
    assert!(bin.exists(), "build holon-agentd first");
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let _d = Daemon(
        std::process::Command::new(bin)
            .args(["--addr", &format!("127.0.0.1:{port}"), "--provider", "mock", "--token", TOKEN])
            .arg("--workspace-root")
            .arg(&root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let url = format!("http://127.0.0.1:{port}");
    let t0 = Instant::now();
    while reqwest::blocking::get(format!("{url}/health")).map_or(true, |r| !r.status().is_success())
    {
        assert!(t0.elapsed() < Duration::from_secs(10), "the daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }

    let mut cfg = Config::new(&state);
    let mut a = AgentdConfig::new(&url);
    a.token = Some(TOKEN.into());
    a.model = "mock".into();
    a.poll = Duration::from_millis(50);
    cfg.agentd = Some(a);
    let rt = Runtime::new(cfg).unwrap();
    rt.start_scheduler();
    let mut lead = AgentSpec::new("lead", "hands coding work to a session");
    for t in ["session_start", "session_result"] {
        lead.capabilities.push(Capability::named(t));
    }
    lead.model = ModelSpec::Mock {
        replies: vec![
            r#"{"tool":"session_start","args":{"task":"look"}}"#.into(),
            r#"{"tool":"session_result","args":{"wait_secs":5}}"#.into(),
            "done".into(),
        ],
    };
    rt.store().put(&lead).unwrap();
    let run = {
        let rt = rt.clone();
        std::thread::spawn(move || rt.run_agent("lead", "http", "go", &Cause::default(), "", true))
    };
    let t0 = Instant::now();
    let p = loop {
        if let Some(p) = rt.pending_approvals().pop() {
            break p;
        }
        let view = rt.sessions().first().cloned();
        assert!(
            view.as_ref().is_none_or(|v| v.phase != Phase::Failed),
            "the session failed: {:?}",
            view.map(|v| v.result)
        );
        assert!(t0.elapsed() < Duration::from_secs(10), "no approval appeared");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(p.tool, "session:list_dir");
    assert!(rt.resolve_approval(p.id, true));
    let rec = run.join().unwrap().unwrap();
    assert!(rec.answer.contains("done"), "{}", rec.answer);
    assert_eq!(rt.sessions()[0].phase, Phase::Done);
}
