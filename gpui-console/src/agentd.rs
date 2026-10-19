//! The coding-session daemon, started beside the console so that an agent granted `session_start`
//! has somewhere to open one (docs/adr/0119).
//!
//! Three ways to have one, in order:
//!
//! * `HOLON_AGENTD_URL` names one that is already running (and `HOLON_AGENTD_TOKEN_FILE` its token):
//!   the console attaches and starts nothing.
//! * `comp-agentd` is built (`HOLON_AGENTD_BIN`, or `reconciler/target/{release,debug}`): the console
//!   starts one on a free port with a random token, confined to the agents' workspaces, and stops it
//!   on exit.
//! * Neither: sessions are off and `session_start` says so.
//!
//! The model a started daemon uses is **`mock` unless the operator says otherwise**: a scripted
//! provider that lists the workspace and answers, free and deterministic, because a console that
//! began spending someone's API key the first time an agent asked for a coding session would be a
//! surprise nobody wanted. `HOLON_AGENTD_PROVIDER` (`anthropic`, `openai`), `HOLON_AGENTD_BASE_URL`,
//! `HOLON_AGENTD_KEY_FILE` and `HOLON_SESSION_MODEL` choose a real one.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use agent_runtime::session::AgentdConfig;
use comp_fleet::fleet::{free_port, repo_root};

/// A `comp-agentd` this process started, stopped when this is dropped.
pub struct AgentdProc {
    child: Child,
    pub config: AgentdConfig,
    /// What it answers with, for the status line: `mock`, or the provider and model.
    pub describes: String,
}

impl Drop for AgentdProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The daemon to start: the one named, else the NEWEST build there is. Not release before debug: a
/// release build from last week is a daemon without the state snapshot a session is watched through,
/// and it answers a state request with a bare 400.
fn binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("HOLON_AGENTD_BIN") {
        return Some(PathBuf::from(p)).filter(|p| p.exists());
    }
    let target = repo_root().join("reconciler/target");
    ["release", "debug"]
        .iter()
        .map(|p| target.join(p).join("comp-agentd"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .max()
        .map(|(_, p)| p)
}

/// Whether the daemon at `url` has what the runtime watches a session through: the state snapshot
/// (ADR-0107). Asked about a session that cannot exist it answers 404 with a problem document; an
/// older daemon, which has no such route, answers something else, and a window that shows a session
/// as "lost the daemon" a second after it was started is a poor way to learn that.
fn probe(url: &str, token: &Option<String>) -> Result<(), String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    let mut rq = http.get(format!("{}/v1/sessions/ses_probe/state", url.trim_end_matches('/')));
    if let Some(t) = token {
        rq = rq.bearer_auth(t);
    }
    let r = rq.send().map_err(|e| format!("comp-agentd at {url} did not answer: {e}"))?;
    let status = r.status().as_u16();
    let body = r.text().unwrap_or_default();
    if status == 404 && body.contains("\"title\"") {
        Ok(())
    } else if status == 401 || status == 403 {
        Err(format!("comp-agentd at {url} refused the token ({status})"))
    } else {
        Err(format!(
            "comp-agentd at {url} has no state snapshot (it answered {status}): it is older than \
             ADR-0107; rebuild it with `cargo build --bin comp-agentd` in reconciler/"
        ))
    }
}

/// A secret for one run: 16 bytes of the system's randomness, in hex.
fn token() -> String {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .expect("the system has a source of randomness");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What to do about the daemon: attach, start, or neither. `workspaces` is where the agents'
/// workspaces are, which is all a started daemon may work under.
pub fn start(
    workspaces: &Path,
    state: &Path,
) -> Result<(AgentdConfig, Option<AgentdProc>), String> {
    if let Ok(url) = std::env::var("HOLON_AGENTD_URL").map(|u| u.trim().to_string()) {
        if !url.is_empty() {
            let mut cfg = AgentdConfig::new(url);
            cfg.token = match std::env::var("HOLON_AGENTD_TOKEN_FILE") {
                Ok(f) => Some(
                    std::fs::read_to_string(&f)
                        .map_err(|e| format!("{f}: {e}"))?
                        .trim()
                        .to_string(),
                ),
                Err(_) => None,
            };
            cfg.model = std::env::var("HOLON_SESSION_MODEL").unwrap_or_default();
            probe(&cfg.url, &cfg.token)?;
            return Ok((cfg, None));
        }
    }

    let bin = binary().ok_or(
        "comp-agentd is not built (cargo build --bin comp-agentd in reconciler/), and \
         HOLON_AGENTD_URL is not set",
    )?;
    std::fs::create_dir_all(workspaces).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(state).map_err(|e| e.to_string())?;
    let workspaces = std::fs::canonicalize(workspaces).map_err(|e| e.to_string())?;

    let provider = std::env::var("HOLON_AGENTD_PROVIDER").unwrap_or_else(|_| "mock".into());
    let model = match (provider.as_str(), std::env::var("HOLON_SESSION_MODEL")) {
        ("mock", _) => "mock".to_string(),
        (_, Ok(m)) if !m.is_empty() => m,
        (p, _) => return Err(format!("HOLON_AGENTD_PROVIDER={p} needs HOLON_SESSION_MODEL")),
    };
    let port = free_port();
    let token = token();
    let log = std::fs::File::create(state.join("agentd.log")).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(bin);
    cmd.args(["--addr", &format!("127.0.0.1:{port}"), "--token", &token, "--provider", &provider])
        .arg("--workspace-root")
        .arg(&workspaces)
        .arg("--state-dir")
        .arg(state);
    if let Ok(base) = std::env::var("HOLON_AGENTD_BASE_URL") {
        cmd.args(["--base-url", &base]);
    }
    if let Ok(key) = std::env::var("HOLON_AGENTD_KEY_FILE") {
        cmd.args(["--api-key-file", &key]);
    }
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|e| format!("could not start comp-agentd: {e}"))?;

    let url = format!("http://127.0.0.1:{port}");
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut up = AgentdProc {
        child,
        describes: if provider == "mock" { "mock".into() } else { format!("{provider} {model}") },
        config: {
            let mut c = AgentdConfig::new(&url);
            c.token = Some(token);
            c.model = model;
            c
        },
    };
    while Instant::now() < deadline {
        if http.get(format!("{url}/health")).send().is_ok_and(|r| r.status().is_success()) {
            // an old build starts and listens and then cannot do the one thing wanted of it
            probe(&url, &up.config.token)?;
            return Ok((up.config.clone(), Some(up)));
        }
        if let Ok(Some(status)) = up.child.try_wait() {
            return Err(format!(
                "comp-agentd exited at start ({status}); see {}",
                state.join("agentd.log").display()
            ));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Err(format!("comp-agentd did not answer on {url} within 20s"))
}
