//! `comp-park` and `comp-resume` on `--replicas` and on a list of servers (ADR-0111), as processes.
//!
//! What the copies buy — a node killed, every acknowledged ticket still there, delivery going on —
//! is `holon-park`'s `tests/cluster.rs`, against a real three-node cluster. This file is the part
//! that file cannot see: the flag reaching the server. A daemon that parsed `--replicas 3` and then
//! dropped it would start without complaint, so the test asks one server that cannot keep three
//! copies and requires the daemon to be refused, with the server's reason, and the same daemon
//! without the flag to start.
//!
//! Needs `nats-server` on `PATH`; without it every test prints `SKIPPED` and returns.

use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// One non-clustered `nats-server -js`, which cannot keep more than one copy of anything.
struct Server {
    child: Child,
    port: u16,
    dir: std::path::PathBuf,
}

impl Server {
    fn start() -> Option<Server> {
        // Under the target directory, which Cargo hands every integration test for exactly this:
        // a fresh, private path rather than a guessable one in the system temp dir.
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "park-replicas-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let port = free_port();
        let child = Command::new("nats-server")
            .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
            .arg(&dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let server = Server { child, port, dir };
        let t0 = Instant::now();
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(t0.elapsed() < Duration::from_secs(15), "nats-server never listened");
            std::thread::sleep(Duration::from_millis(50));
        }
        Some(server)
    }

    fn url(&self) -> String {
        format!("nats://127.0.0.1:{}", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

macro_rules! server_or_skip {
    ($what:expr) => {
        match Server::start() {
            Some(s) => s,
            None => {
                eprintln!("SKIPPED {}: no `nats-server` on PATH", $what);
                return;
            }
        }
    };
}

fn tag() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    )
}

/// Run a daemon to its end, or kill it if it is still going after `secs`.
fn run_to_end(mut cmd: Command, secs: u64) -> (Output, bool) {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn");
    let t0 = Instant::now();
    let mut killed = false;
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            let _ = child.kill();
            killed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    (child.wait_with_output().unwrap(), killed)
}

fn park(url: &str, port: u16, tag: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_comp-park"));
    cmd.args(["--addr", &format!("127.0.0.1:{port}")])
        .args(["--nats-url", url])
        .args(["--bucket", &format!("replicas-{tag}")])
        .args(["--wake-stream", &format!("REPLICAS_{}", tag.replace('-', "_"))]);
    cmd
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// `comp-park` started and answering `/health`, for as long as it takes to ask.
fn healthy(mut cmd: Command, port: u16) -> bool {
    let mut child = cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("spawn");
    let http =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
    let t0 = Instant::now();
    let mut up = false;
    while t0.elapsed() < Duration::from_secs(20) {
        if let Ok(r) = http.get(format!("http://127.0.0.1:{port}/health")).send() {
            if r.json::<serde_json::Value>().map(|v| v["ok"] == true).unwrap_or(false) {
                up = true;
                break;
            }
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    up
}

#[test]
fn comp_park_asks_the_server_for_the_copies_it_was_given() {
    let server = server_or_skip!("comp_park_asks_the_server_for_the_copies_it_was_given");
    let tag = tag();

    // Without the flag it starts: whatever is wrong below is the flag's doing.
    let port = free_port();
    assert!(
        healthy(park(&server.url(), port, &format!("{tag}-ctl")), port),
        "comp-park does not start on this server even without the flag"
    );

    // With it, one server cannot oblige, and says so. A daemon that dropped the flag would have
    // started instead.
    let mut cmd = park(&server.url(), free_port(), &format!("{tag}-3"));
    cmd.args(["--replicas", "3"]);
    let (out, killed) = run_to_end(cmd, 20);
    assert!(!killed, "comp-park --replicas 3 is still running: the flag never reached the server");
    assert!(!out.status.success(), "{:?}", out.status);
    let err = stderr(&out);
    assert!(err.contains("3 copies"), "{err}");
    assert!(err.contains("non-clustered"), "the server's own reason: {err}");
}

#[test]
fn comp_resume_asks_the_server_for_the_copies_it_was_given() {
    let server = server_or_skip!("comp_resume_asks_the_server_for_the_copies_it_was_given");
    let tag = tag();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_comp-resume"));
    cmd.args(["--sink-url", "http://127.0.0.1:1/deliver"])
        .args(["--nats-url", &server.url()])
        .args(["--bucket", &format!("replicas-{tag}")])
        .args(["--wake-stream", &format!("REPLICAS_{}", tag.replace('-', "_"))])
        .args(["--replicas", "3"]);
    let (out, killed) = run_to_end(cmd, 20);
    assert!(
        !killed,
        "comp-resume --replicas 3 is still running: the flag never reached the server"
    );
    assert!(!out.status.success(), "{:?}", out.status);
    let err = stderr(&out);
    assert!(err.contains("3 copies") && err.contains("non-clustered"), "{err}");
}

#[test]
fn a_server_that_is_down_is_passed_over_in_a_list() {
    let server = server_or_skip!("a_server_that_is_down_is_passed_over_in_a_list");
    let dead = format!("nats://127.0.0.1:{}", free_port());
    let port = free_port();
    let cmd = park(&format!("{dead},{}", server.url()), port, &tag());
    assert!(healthy(cmd, port), "comp-park never came up with the live server second in the list");
}

#[test]
fn a_replica_count_that_cannot_be_is_a_usage_error() {
    // No server needed: these are refused before anything is connected.
    for n in ["0", "6"] {
        let out =
            Command::new(env!("CARGO_BIN_EXE_comp-park")).args(["--replicas", n]).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "--replicas {n}: {}", stderr(&out));
        assert!(stderr(&out).contains("1..=5"), "--replicas {n}: {}", stderr(&out));
    }
    let out = Command::new(env!("CARGO_BIN_EXE_comp-park"))
        .args(["--memory", "--replicas", "3"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("cannot be used with"), "memory has no copies: {}", stderr(&out));
}
