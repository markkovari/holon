//! `comp-agentd` parking its approvals (ADR-0113), through the real daemons: `comp-agentd`,
//! `comp-park` and `comp-resume` as processes, over a private `nats-server`, a token on every hop.
//!
//! The unit tests in `agentd/tests.rs` play `comp-park` and `comp-resume` themselves. This is the
//! chain with nobody playing: the answer goes agentd -> `comp-park` -> NATS -> `comp-resume` ->
//! agentd's sink, and the thing it proves is what no stand-in can, that a task waiting on a person
//! survives `kill -9` of the daemon that asked.
//!
//! Needs `nats-server` on `PATH`; without it every test prints `SKIPPED` and returns.

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const AGENT_TOKEN: &str = "agent-secret";
const PARK_TOKEN: &str = "park-secret";

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A process this test started, killed when it goes out of scope.
struct Proc {
    child: Child,
    log: PathBuf,
}

impl Proc {
    fn start(program: impl AsRef<std::ffi::OsStr>, args: &[String], log: PathBuf) -> Option<Proc> {
        let out = std::fs::File::create(&log).unwrap();
        let child = Command::new(program)
            .args(args)
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .stdin(Stdio::null())
            .spawn()
            .ok()?;
        Some(Proc { child, log })
    }

    /// SIGKILL: no flush, no goodbye.
    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn tail(&self) -> String {
        let t = std::fs::read_to_string(&self.log).unwrap_or_default();
        t.lines()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Stack {
    dir: PathBuf,
    ws: PathBuf,
    state: PathBuf,
    agent_port: u16,
    park_port: u16,
    approval_timeout: Option<u64>,
    nats: Option<Proc>,
    park: Option<Proc>,
    resume: Option<Proc>,
    agentd: Option<Proc>,
    http: reqwest::blocking::Client,
}

fn wait_listening(port: u16, what: &str) {
    let t0 = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(t0.elapsed() < Duration::from_secs(20), "{what} never listened on {port}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

impl Stack {
    /// Everything but agentd. `None` when there is no `nats-server` to start.
    fn up(what: &str, approval_timeout: Option<u64>) -> Option<Stack> {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "agentd-park-{what}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        let (ws, state) = (dir.join("workspace"), dir.join("state"));
        for d in [&ws, &state, &dir.join("nats")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(ws.join("README.md"), "hi").unwrap();
        let (nats_port, park_port, agent_port) = (free_port(), free_port(), free_port());
        let mut s = Stack {
            ws: ws.canonicalize().unwrap(),
            state,
            agent_port,
            park_port,
            approval_timeout,
            nats: None,
            park: None,
            resume: None,
            agentd: None,
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
            dir,
        };
        let a = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        s.nats = Some(Proc::start(
            "nats-server",
            &a(&[
                "-js",
                "-a",
                "127.0.0.1",
                "-p",
                &nats_port.to_string(),
                "-sd",
                s.dir.join("nats").to_str().unwrap(),
            ]),
            s.dir.join("nats.log"),
        )?);
        wait_listening(nats_port, "nats-server");
        let nats = format!("nats://127.0.0.1:{nats_port}");
        s.park = Proc::start(
            env!("CARGO_BIN_EXE_comp-park"),
            &a(&[
                "--addr",
                &format!("127.0.0.1:{park_port}"),
                "--nats-url",
                &nats,
                "--bucket",
                "ag",
                "--wake-stream",
                "AG_WAKE",
                "--token",
                PARK_TOKEN,
            ]),
            s.dir.join("park.log"),
        );
        wait_listening(park_port, "comp-park");
        s.resume = Proc::start(
            env!("CARGO_BIN_EXE_comp-resume"),
            &a(&[
                "--nats-url",
                &nats,
                "--bucket",
                "ag",
                "--wake-stream",
                "AG_WAKE",
                "--sink-url",
                &format!("http://127.0.0.1:{agent_port}/park/deliver"),
                "--sink-token",
                AGENT_TOKEN,
                "--sweep-secs",
                "1",
                "--deadline-grace-secs",
                "0",
                "--reannounce-after-secs",
                "3600",
            ]),
            s.dir.join("resume.log"),
        );
        Some(s)
    }

    /// (Re)start agentd on the same state directory, which is what a restart is.
    fn start_agentd(&mut self, n: usize) {
        self.start_agentd_with(n, &[]);
    }

    /// The same, with more flags on its command line.
    fn start_agentd_with(&mut self, n: usize, extra: &[&str]) {
        let mut args: Vec<String> = [
            "--addr",
            &format!("127.0.0.1:{}", self.agent_port),
            "--workspace-root",
            self.ws.to_str().unwrap(),
            "--state-dir",
            self.state.to_str().unwrap(),
            "--provider",
            "mock",
            "--token",
            AGENT_TOKEN,
            "--park-url",
            &format!("http://127.0.0.1:{}", self.park_port),
            "--park-token",
            PARK_TOKEN,
        ]
        .iter()
        .map(|x| x.to_string())
        .collect();
        if let Some(t) = self.approval_timeout {
            args.extend(["--approval-timeout-secs".into(), t.to_string()]);
        }
        args.extend(extra.iter().map(|x| x.to_string()));
        self.agentd = Proc::start(
            env!("CARGO_BIN_EXE_comp-agentd"),
            &args,
            self.dir.join(format!("agentd-{n}.log")),
        );
        wait_listening(self.agent_port, "comp-agentd");
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.agent_port)
    }

    fn get(&self, path: &str) -> Value {
        let r = self.http.get(self.url(path)).bearer_auth(AGENT_TOKEN).send().unwrap();
        r.json().unwrap()
    }

    fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self.http.post(self.url(path)).bearer_auth(AGENT_TOKEN).json(&body).send().unwrap();
        (r.status().as_u16(), r.json().unwrap_or(Value::Null))
    }

    fn session(&self) -> String {
        let (s, v) = self.post(
            "/v1/sessions",
            json!({"model": "mock", "workspace_dir": self.ws, "max_budget_usd_micros": 1_000_000}),
        );
        assert_eq!(s, 201, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    /// Poll `f` until it gives something, or fail with what every daemon last said.
    fn until<T>(&self, what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
        let t0 = Instant::now();
        loop {
            if let Some(v) = f() {
                return v;
            }
            if t0.elapsed() > Duration::from_secs(secs) {
                let logs = [
                    ("agentd", &self.agentd),
                    ("comp-park", &self.park),
                    ("comp-resume", &self.resume),
                ]
                .iter()
                .map(|(n, p)| {
                    format!("--- {n}\n{}", p.as_ref().map(|p| p.tail()).unwrap_or_default())
                })
                .collect::<Vec<_>>()
                .join("\n");
                panic!("timed out waiting for {what}\n{logs}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The call a task is waiting on a person for, if it is.
    fn waiting_call(&self, sid: &str) -> Option<String> {
        let st = self.get(&format!("/v1/sessions/{sid}/state"));
        st["pending_approvals"]
            .as_array()?
            .first()
            .map(|a| a["call_id"].as_str().unwrap().to_string())
    }

    /// How the last task ended, once it has.
    fn ended(&self, sid: &str) -> Option<Value> {
        let st = self.get(&format!("/v1/sessions/{sid}/state"));
        st["transcript"].as_array()?.iter().rev().find(|e| e["type"] == "task_ended").cloned()
    }

    /// How the `n`th task ended (1-based), once it has.
    fn nth_ended(&self, sid: &str, n: usize) -> Option<Value> {
        let st = self.get(&format!("/v1/sessions/{sid}/state"));
        st["transcript"]
            .as_array()?
            .iter()
            .filter(|e| e["type"] == "task_ended")
            .nth(n - 1)
            .cloned()
    }

    /// The tool calls of a session's transcript, in order.
    fn calls(&self, sid: &str) -> Vec<Value> {
        let st = self.get(&format!("/v1/sessions/{sid}/state"));
        let all = st["transcript"].as_array().cloned().unwrap_or_default();
        all.into_iter().filter(|e| e["type"] == "tool_call").collect()
    }
}

macro_rules! stack_or_skip {
    ($what:expr, $timeout:expr) => {
        match Stack::up($what, $timeout) {
            Some(s) => s,
            None => {
                eprintln!("SKIPPED {}: no `nats-server` on PATH", $what);
                return;
            }
        }
    };
}

#[test]
fn a_task_waiting_for_a_person_survives_kill_dash_9_of_agentd_and_finishes_when_they_answer() {
    let mut s = stack_or_skip!("survives", None);
    s.start_agentd(1);
    let sid = s.session();
    let (code, task) = s.post(&format!("/v1/sessions/{sid}/tasks"), json!({"prompt": "look"}));
    assert_eq!((code, task["status"].as_str()), (202, Some("running")));
    let call_id = s.until("the task to wait for a person", 15, || s.waiting_call(&sid));
    assert!(s.state.join(&sid).join("checkpoint.json").exists(), "written when it parked");
    assert_eq!(s.get(&format!("/v1/sessions/{sid}"))["status"], "awaiting_approval");

    // The daemon that asked dies, with no warning and no flush.
    s.agentd.take().unwrap().kill();
    s.start_agentd(2);

    // Its replacement finds the task waiting, not failed: the approval is still pending, and the
    // log has no ending for the task.
    let again = s
        .until("the new process to know what the task is waiting for", 15, || s.waiting_call(&sid));
    assert_eq!(again, call_id);
    assert!(s.ended(&sid).is_none(), "the restart did not cut the task off");
    assert_eq!(s.get(&format!("/v1/sessions/{sid}"))["status"], "awaiting_approval");

    // A person answers the new process. The answer goes to `comp-park`, through NATS to
    // `comp-resume`, and back to this daemon's sink, which moves the task on.
    let (code, ans) = s.post(
        &format!("/v1/sessions/{sid}/tool-calls/{call_id}/approval"),
        json!({"decision": "approve", "approver": "mark"}),
    );
    assert_eq!((code, ans["decision"].as_str()), (200, Some("approve")), "{ans}");
    let ended = s.until("the task to finish", 20, || s.ended(&sid));
    assert_eq!(ended["outcome"], "completed", "{ended}");
    assert_eq!(ended["result"], "The workspace holds: README.md.");
    // Two model turns, one on each side of the kill: the spend of the first was not lost with it.
    assert_eq!(ended["usage"]["cost_usd_micros"], 37_500, "{ended}");
    assert!(!s.state.join(&sid).join("checkpoint.json").exists(), "gone when the task moved on");
}

#[test]
fn an_approval_nobody_answers_is_interrupted_by_the_resumers_pass_and_is_a_denial() {
    let mut s = stack_or_skip!("timeout", Some(2));
    s.start_agentd(1);
    let sid = s.session();
    s.post(&format!("/v1/sessions/{sid}/tasks"), json!({"prompt": "look"}));
    let call_id = s.until("the task to wait for a person", 15, || s.waiting_call(&sid));

    // Nobody answers. The ticket's deadline passes, `comp-resume`'s recovery pass interrupts it
    // and delivers that to the sink, which is a denial: the model is told, and the task ends.
    let ended = s.until("the approval to time out", 30, || s.ended(&sid));
    assert_eq!(ended["outcome"], "completed", "{ended}");
    assert_eq!(ended["result"], "The workspace holds: denied by a human: nobody answered in time.");
    let st = s.get(&format!("/v1/sessions/{sid}/state"));
    let call = st["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "tool_call" && e["call_id"] == call_id.as_str())
        .unwrap();
    assert_eq!(
        (call["decision"].as_str(), call["decided_by"].as_str()),
        (Some("deny"), Some("timeout"))
    );
}

#[test]
fn an_approval_for_the_session_crosses_the_real_chain_and_kill_dash_9_and_a_flag_allows_a_tool_unasked(
) {
    let mut s = stack_or_skip!("scope", None);
    s.start_agentd(1);
    let sid = s.session();
    let (code, _) = s.post(&format!("/v1/sessions/{sid}/tasks"), json!({"prompt": "look"}));
    assert_eq!(code, 202);
    let call_id = s.until("the task to wait for a person", 15, || s.waiting_call(&sid));

    // The person approves the tool for the session. The answer and its reach go to `comp-park`,
    // through NATS to `comp-resume`, and back; and the daemon says what it understood.
    let (code, ans) = s.post(
        &format!("/v1/sessions/{sid}/tool-calls/{call_id}/approval"),
        json!({"decision": "approve", "approver": "mark", "scope": "session"}),
    );
    assert_eq!((code, ans["scope"].as_str()), (200, Some("session")), "{ans}");
    let ended = s.until("the first task to finish", 20, || s.nth_ended(&sid, 1));
    assert_eq!(ended["outcome"], "completed", "{ended}");
    let view = format!("/v1/sessions/{sid}");
    assert_eq!(s.get(&view)["approved_tools"], json!(["list_dir"]));

    // The daemon dies, and its replacement has the approval back from the log.
    s.agentd.take().unwrap().kill();
    s.start_agentd(2);
    assert_eq!(s.get(&view)["approved_tools"], json!(["list_dir"]), "read back from the log");

    // A second task calls the tool and nobody is asked: it parks nothing, and runs.
    let (code, _) = s.post(&format!("/v1/sessions/{sid}/tasks"), json!({"prompt": "look again"}));
    assert_eq!(code, 202);
    let ended = s.until("the second task to finish unasked", 20, || s.nth_ended(&sid, 2));
    assert_eq!(ended["outcome"], "completed", "{ended}");
    assert!(s.waiting_call(&sid).is_none());
    let calls = s.calls(&sid);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[1]["decision"], Value::Null, "the second call was never put to a person");

    // A daemon told to allow the tool never asks about it, and a session created there says so.
    s.agentd.take().unwrap().kill();
    s.start_agentd_with(3, &["--auto-approve-tool", "list_dir"]);
    let fresh = s.session();
    let created = s.get(&format!("/v1/sessions/{fresh}"));
    assert_eq!(created["auto_approve_tools"], json!(["list_dir"]));
    assert_eq!(created["approved_tools"], json!([]), "an allowance is not an approval");
    s.post(&format!("/v1/sessions/{fresh}/tasks"), json!({"prompt": "look"}));
    let ended = s.until("the task to finish unasked", 20, || s.ended(&fresh));
    assert_eq!(ended["outcome"], "completed", "{ended}");
    assert_eq!(s.calls(&fresh)[0]["decision"], Value::Null, "{:?}", s.calls(&fresh));
}
