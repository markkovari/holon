//! `comp-checks` against a real tree, running real commands.
//!
//! The gate is the one part of the loop that cannot be a component — it has to
//! run the project's own tests, and a component cannot spawn a process. So this
//! is the test that the native half does what ADR-0081 says: report a CHECK
//! VECTOR rather than a verdict, so the caller gets both halves.
//!
//!   * every `required` check passed  → the gate. May this be accepted?
//!   * the weighted fraction passed   → the score. Which candidate to extend?
//!
//! A binary gate gives no gradient in the generation where nothing passes yet,
//! which is the generation a search most needs to make progress in.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use comp_reconciler::fleet::{bin_path, free_port};
use serde_json::{json, Value};

/// A runner that dies with the test.
struct Runner {
    child: Child,
    port: u16,
    _dir: tempfile::TempDir,
}

impl Drop for Runner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Runner {
    /// A base tree with one file in it, and a runner pointed at it.
    fn start(allow: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        std::fs::create_dir_all(&base).unwrap();
        // Something for a check to find, so "the candidate's files are there" is
        // distinguishable from "the base was copied".
        std::fs::write(base.join("VERSION"), "base\n").unwrap();

        let port = free_port();
        let mut cmd = Command::new(bin_path("comp-checks"));
        cmd.args(["--addr", &format!("127.0.0.1:{port}")])
            .arg("--base")
            .arg(&base)
            .args(["--timeout", "30"]);
        for a in allow {
            cmd.args(["--allow", a]);
        }
        let child = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().expect("comp-checks");

        let me = Self { child, port, _dir: dir };
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return me;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("comp-checks never listened on {port}");
    }

    /// A runner with NO checkout — the shape that is not pinned to a machine
    /// holding the repository.
    fn without_checkout(allow: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let port = free_port();
        let mut cmd = Command::new(bin_path("comp-checks"));
        cmd.args(["--addr", &format!("127.0.0.1:{port}")])
            .arg("--work-dir")
            .arg(dir.path())
            .args(["--timeout", "30"]);
        for a in allow {
            cmd.args(["--allow", a]);
        }
        let child = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().expect("comp-checks");
        let me = Self { child, port, _dir: dir };
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return me;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("comp-checks never listened on {port}");
    }

    /// POST a candidate and read the report. Hand-rolled because the runner is
    /// deliberately a few hundred lines of std and does not deserve a client.
    fn evaluate(&self, body: Value) -> Value {
        // A dropped CONNECTION is retried; a REPORT never is.
        //
        // The same rule as `staleness.rs`: what this file measures is what the
        // runner said, and a refused or reset connection said nothing at all.
        // Under a loaded machine one connection in a few hundred is reset, and a
        // single one used to fail the whole file. Retrying an actual report
        // would instead retry away the verdict under test.
        // Budgeted by TIME, not by a count. Five attempts at 200ms is one second,
        // which is fine on an idle machine and not fine while the rest of the suite
        // is compiling 150 crates around it — `Connection refused, attempt 5/5`
        // failed a full run that passed in isolation thirty seconds later. The same
        // mistake as `--timeout 300`: a bound measured idle and spent under load.
        let payload = body.to_string();
        let mut last = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut attempt = 0;
        while std::time::Instant::now() < deadline {
            attempt += 1;
            match Self::once(self.port, &payload) {
                Ok(v) => return v,
                Err(e) => {
                    last = format!("attempt {attempt} over {:?}: {e}", deadline.elapsed());
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
        panic!("the runner never answered — {last}");
    }

    fn once(port: u16, payload: &str) -> std::result::Result<Value, String> {
        let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
        s.write_all(
            format!(
                "POST /check HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                payload.len()
            )
            .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        let mut out = String::new();
        s.read_to_string(&mut out).map_err(|e| e.to_string())?;
        let body = out.split("\r\n\r\n").nth(1).unwrap_or_default();
        serde_json::from_str(body).map_err(|e| format!("unreadable report ({e}): {out}"))
    }

}

/// `sh -c` is not on any allow-list in this file; these use real binaries.
fn check(id: &str, required: bool, weight: u32, command: &[&str]) -> Value {
    json!({
        "id": id, "required": required, "weight": weight,
        "command": command.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    })
}

#[test]
fn the_gate_and_the_score_come_back_from_a_real_run() {
    let runner = Runner::start(&["test", "cat", "false"]);

    // --- a candidate that passes everything ----------------------------------
    let report = runner.evaluate(json!({
        "candidate": "branch-1",
        "changes": [{ "path": "answer.txt", "content": "42\n" }],
        "checks": [
            // The candidate's own file has to be there, which is what makes this
            // a check of the CANDIDATE rather than of the base tree.
            check("the-change-landed", true, 1, &["test", "-f", "answer.txt"]),
            check("the-base-is-there", true, 1, &["test", "-f", "VERSION"]),
        ],
    }));
    assert_eq!(report["accepted"], json!(true), "everything passed: {report}");
    assert_eq!(report["score"], json!(1000), "a full pass is 1000 milli-units: {report}");
    assert_eq!(report["passed"], json!(2));

    // --- a candidate that fails the gate but is not worthless ----------------
    // This is the case a binary verdict cannot express, and the reason the runner
    // reports a vector.
    let report = runner.evaluate(json!({
        "candidate": "branch-2",
        "changes": [{ "path": "answer.txt", "content": "42\n" }],
        "checks": [
            check("the-change-landed", true, 1, &["test", "-f", "answer.txt"]),
            check("the-hard-part", true, 1, &["test", "-f", "not-written-yet.txt"]),
            check("a-nice-to-have", false, 1, &["test", "-f", "VERSION"]),
        ],
    }));
    assert_eq!(report["accepted"], json!(false), "a required check failed: {report}");
    let partial = report["score"].as_u64().unwrap();
    assert!(
        partial > 0 && partial < 1000,
        "a partial pass must score between the two, or a generation where nothing is \
         acceptable has nothing to select on: {report}"
    );

    // --- and a worse candidate scores lower ----------------------------------
    let worse = runner.evaluate(json!({
        "candidate": "branch-3",
        "changes": [],
        "checks": [
            check("the-change-landed", true, 1, &["test", "-f", "answer.txt"]),
            check("the-hard-part", true, 1, &["test", "-f", "not-written-yet.txt"]),
            check("a-nice-to-have", false, 1, &["test", "-f", "VERSION"]),
        ],
    }));
    assert!(
        worse["score"].as_u64().unwrap() < partial,
        "a candidate that landed nothing must score below one that landed something — \
         that ordering IS the selection signal: {worse} vs {partial}"
    );

    // --- candidates do not see each other ------------------------------------
    // branch-3 wrote nothing, and branch-1's file must not have been visible to
    // it. If the tree were reused, `the-change-landed` would have passed above.
    assert_eq!(
        worse["results"][0]["passed"],
        json!(false),
        "a later candidate saw an earlier one's files — every candidate gets its own \
         tree or none of the scores mean anything: {worse}"
    );
}

#[test]
fn a_command_nobody_allowed_is_refused_rather_than_run() {
    let runner = Runner::start(&["test"]);

    let report = runner.evaluate(json!({
        "candidate": "hostile",
        "changes": [],
        "checks": [
            check("ok", false, 1, &["test", "-f", "VERSION"]),
            // The input is written by an agent. `rm` is not on the list.
            check("sneaky", true, 1, &["rm", "-rf", "."]),
        ],
    }));

    let sneaky = &report["results"][1];
    assert_eq!(sneaky["passed"], json!(false), "an unlisted command must not pass: {report}");
    assert!(
        sneaky["detail"].as_str().unwrap_or_default().contains("allow-list"),
        "the report must say WHY, or a refused check reads as a broken one: {sneaky}"
    );
    assert_eq!(report["accepted"], json!(false));
    // The other check still ran: one bad entry must not discard the rest, or a
    // single typo costs a whole evaluation.
    assert_eq!(report["results"][0]["passed"], json!(true), "the allowed check still ran");
}

#[test]
fn a_check_that_hangs_is_killed_rather_than_holding_the_runner() {
    // `sleep` is allowed on purpose: the point is that a check the operator DID
    // permit can still run forever, which an infinite loop in generated code
    // plausibly does.
    let runner = Runner::start(&["sleep", "test"]);

    let started = Instant::now();
    let report = runner.evaluate(json!({
        "candidate": "hangs",
        "changes": [],
        "checks": [check("forever", true, 1, &["sleep", "600"])],
    }));
    let took = started.elapsed();

    assert_eq!(report["accepted"], json!(false), "a killed check has not passed: {report}");
    assert!(
        took < Duration::from_secs(90),
        "the runner waited {took:?} on a check with a 30s timeout — one hung candidate \
         would stall every other branch behind it"
    );
    assert!(
        report["results"][0]["detail"].as_str().unwrap_or_default().contains("killed"),
        "the report should say it was killed rather than that it failed: {report}"
    );

    // And the runner is still usable afterwards.
    let after = runner.evaluate(json!({
        "candidate": "after",
        "changes": [],
        "checks": [check("still-works", true, 1, &["test", "-f", "VERSION"])],
    }));
    assert_eq!(after["accepted"], json!(true), "the runner died with the check it killed");
}

/// A runner with no checkout at all, fed from the object store by its caller.
///
/// This is the shape that matters. `--base` pins a runner to a machine that
/// already has the repository, which is the opposite of what putting the
/// repository in blob storage was for. A caller that can read `vgit:store` reads
/// the tree from NATS and posts it — and because a commit id is a content
/// address, the runner caches it under that name and every later candidate on the
/// same base sends only its diff.
///
/// Disk is still where compiling happens, because a compiler cannot read a KV
/// bucket. It is a materialisation, not the authority.
#[test]
fn a_runner_with_no_checkout_is_fed_its_tree_and_caches_it() {
    let runner = Runner::without_checkout(&["test"]);

    let commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    // Without the tree, the runner ASKS for it rather than guessing. A runner
    // that fell back to something else here would score a candidate against the
    // wrong tree and report it confidently.
    let asked = runner.evaluate(json!({
        "candidate": "first",
        "base_commit": commit,
        "checks": [check("base", true, 1, &["test", "-f", "VERSION"])],
    }));
    assert_eq!(
        asked["need_base_tree"],
        json!(true),
        "an unknown base must be asked for, not assumed: {asked}"
    );

    // Sent once.
    let first = runner.evaluate(json!({
        "candidate": "first",
        "base_commit": commit,
        "base_tree": [{ "path": "VERSION", "content": "from-the-object-store\n" }],
        "changes": [{ "path": "answer.txt", "content": "42\n" }],
        "checks": [
            check("base", true, 1, &["test", "-f", "VERSION"]),
            check("candidate", true, 1, &["test", "-f", "answer.txt"]),
        ],
    }));
    assert_eq!(first["accepted"], json!(true), "the posted tree should be usable: {first}");

    // And every candidate after it sends only its own diff — the base is cached
    // under a content address, so there is nothing to invalidate.
    let second = runner.evaluate(json!({
        "candidate": "second",
        "base_commit": commit,
        "changes": [{ "path": "other.txt", "content": "x\n" }],
        "checks": [
            check("base", true, 1, &["test", "-f", "VERSION"]),
            check("mine", true, 1, &["test", "-f", "other.txt"]),
            // The FIRST candidate's file must not be here. A cached base that
            // accumulated candidates would make every later score wrong.
            check("clean", true, 1, &["test", "!", "-f", "answer.txt"]),
        ],
    }));
    assert_eq!(
        second["accepted"],
        json!(true),
        "a cached base must be reused clean, without the previous candidate in it: {second}"
    );

    println!("    no checkout: tree posted once, cached by commit, reused clean");
}
