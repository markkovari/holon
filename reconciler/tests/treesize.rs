//! What a run may ship, and that both ends agree about it.
//!
//! The base tree travels to the driver as ONE message. NATS refuses one past
//! `max_payload`, and the failure at this end is opaque — so `comp-goalrun`
//! refuses it first, with a message that says what to do.
//!
//! The bug this exists to prevent is the two numbers drifting. The guard was
//! `900_000` written into `goalrun`, and the server was started with NATS's own
//! 1 MB default; raise one and the other silently keeps refusing. So the ceiling
//! lives in `fleet`, the server is configured FROM it, and the guard reads it.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use comp_reconciler::fleet::{max_tree_payload, Fleet, MAX_TREE_BYTES};

/// The guard sits below the ceiling it is derived from.
///
/// A guard set AT the ceiling passes a tree that then makes the message carrying
/// it too large — the contract, the goal text and the framing ride with it. That
/// failure lands on the server, where the reason is not visible from here.
#[test]
fn the_guard_sits_below_the_ceiling_it_is_derived_from() {
    assert!(
        max_tree_payload() < MAX_TREE_BYTES,
        "the guard must leave room for the envelope: {} vs {MAX_TREE_BYTES}",
        max_tree_payload()
    );
    // And well above what the old hardcoded 900_000 allowed, or this bought
    // nothing. The largest goal in `.comp/goals` ships 504 KB — 57% of that guard.
    assert!(
        max_tree_payload() > 4 * 1024 * 1024,
        "8 MB was the point; the guard is {}",
        max_tree_payload()
    );
}

/// The SERVER advertises the ceiling this code believes in.
///
/// The number that binds is the one the server was started with, not the one this
/// crate holds — and those were different until the constant moved into `fleet`.
/// Read off the wire rather than from the config the fleet wrote, because a
/// config `nats-server` rejected or ignored still sits on disk looking correct.
///
/// NATS sends `INFO {json}` as the first line on connect, before anything is
/// sent to it, so this needs no client library and no protocol beyond `read`.
#[test]
fn the_server_the_fleet_starts_advertises_that_ceiling() {
    let fleet = Fleet::start("treesize", &[], 1, None);
    let addr = fleet.nats_url.strip_prefix("nats://").expect("a nats:// url").to_string();

    let mut s = TcpStream::connect(&addr).expect("connecting to the fleet's nats");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    // Until the end of the INFO line, not until EOF: the server holds the
    // connection open waiting for CONNECT, so reading to the end never returns.
    while !buf.windows(2).any(|w| w == b"\r\n") {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => panic!("reading INFO from {addr}: {e}"),
        }
    }
    let _ = s.write_all(b"CONNECT {\"verbose\":false}\r\n");

    let line = String::from_utf8_lossy(&buf);
    let json = line
        .strip_prefix("INFO ")
        .unwrap_or_else(|| panic!("the first line was not INFO: {line:?}"));
    let info: serde_json::Value = serde_json::from_str(json.trim()).expect("INFO is json");
    let advertised = info["max_payload"].as_u64().expect("INFO carries max_payload");

    assert_eq!(
        advertised as usize, MAX_TREE_BYTES,
        "the fleet started a server whose ceiling is not the one the guard reads — \
         which is the drift this file exists to catch. INFO said {advertised}."
    );
    // Named explicitly, because passing this while still on the default is the
    // one way this test could be green and worthless.
    assert!(
        advertised > 1024 * 1024,
        "still on NATS's 1 MB default: the config the fleet wrote was not applied"
    );
}

/// Seeding the runner is what lets every plan afterwards carry no tree.
///
/// The claim is not that `seed_base` returns Ok — it is that a candidate judged
/// AFTERWARDS, sending no tree at all, is judged against the real base. That is
/// the property every plan in `goalrun` now depends on, and the one that was
/// previously a side effect of a critic which is allowed to fail.
#[test]
fn a_seeded_runner_judges_a_candidate_that_carries_no_tree() {
    use comp_reconciler::compose;
    use serde_json::json;

    let dir = tempfile::tempdir().unwrap();
    let port = comp_reconciler::fleet::free_port();
    let mut child = std::process::Command::new(comp_reconciler::fleet::bin_path("comp-checks"))
        .args(["--addr", &format!("127.0.0.1:{port}")])
        .arg("--work-dir")
        .arg(dir.path())
        .args(["--allow", "test", "--allow", "grep", "--timeout", "30"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("comp-checks");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let url = format!("http://127.0.0.1:{port}/check");
    let commit = "cccc000000000000000000000000000000000003";
    let tree = json!([
        { "path": "VERSION", "content": "base\n" },
        { "path": "keep/me.txt", "content": "here\n" },
    ]);
    let checks = json!([
        { "id": "base-is-there", "required": true, "weight": 1,
          "command": ["test", "-f", "VERSION"] },
        { "id": "the-change-landed", "required": true, "weight": 1,
          "command": ["test", "-f", "answer.txt"] },
    ]);

    // Before the seed the runner has nothing, and says so rather than guessing.
    let cold =
        compose::gate(&url, None, commit, &json!([]), &json!([]), &checks, Duration::from_secs(30));
    assert!(cold.is_ok(), "the runner did not answer at all: {cold:?}");
    assert!(!cold.unwrap().passed, "a cold runner cannot have passed the base check");

    compose::seed_base(&url, None, commit, &tree, Duration::from_secs(60))
        .expect("seeding the base");

    // The candidate, with NO tree — exactly what a plan now carries.
    let report = compose::gate(
        &url,
        None,
        commit,
        &json!([]),
        &json!([{ "path": "answer.txt", "content": "42\n" }]),
        &checks,
        Duration::from_secs(60),
    )
    .expect("judging against the seeded base");
    assert!(
        report.passed,
        "a seeded runner must judge a treeless candidate against the real base, and said: {:?}",
        report.failures
    );

    // An UNSEEDED commit still asks, so the seed is doing the work rather than
    // the runner having quietly fallen back to something.
    let other = compose::gate(
        &url,
        None,
        "dddd000000000000000000000000000000000004",
        &json!([]),
        &json!([]),
        &checks,
        Duration::from_secs(30),
    );
    assert!(
        !other.expect("an answer").passed,
        "an unseeded commit was judged against somebody else's tree"
    );

    let _ = child.kill();
    let _ = child.wait();
}
