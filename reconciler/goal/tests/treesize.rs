//! The tree-size guard reads the same ceiling the NATS server is configured from.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use holon_fleet::fleet::{max_tree_payload, Fleet, MAX_TREE_BYTES};

/// At the ceiling, the framed message would fail on the server where the reason is invisible.
#[test]
fn the_guard_sits_below_the_ceiling_it_is_derived_from() {
    assert!(
        max_tree_payload() < MAX_TREE_BYTES,
        "the guard must leave room for the envelope: {} vs {MAX_TREE_BYTES}",
        max_tree_payload()
    );
    // The largest goal in `.comp/goals` ships 504 KB.
    assert!(
        max_tree_payload() > 4 * 1024 * 1024,
        "8 MB was the point; the guard is {}",
        max_tree_payload()
    );
}

/// Read off the wire (NATS sends `INFO {json}` first): a rejected config still sits on disk.
#[test]
fn the_server_the_fleet_starts_advertises_that_ceiling() {
    let fleet = Fleet::start("treesize", &[], 1, None);
    let addr = fleet.nats_url.strip_prefix("nats://").expect("a nats:// url").to_string();

    let mut s = TcpStream::connect(&addr).expect("connecting to the fleet's nats");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    // Until the INFO line ends: the server holds the connection open waiting for CONNECT.
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
    // Explicit, since passing on the default would be green and worthless.
    assert!(
        advertised > 1024 * 1024,
        "still on NATS's 1 MB default: the config the fleet wrote was not applied"
    );
}

/// A candidate judged after seeding, sending no tree, is judged against the real base.
#[test]
fn a_seeded_runner_judges_a_candidate_that_carries_no_tree() {
    use holon_goal::compose;
    use serde_json::json;

    let dir = tempfile::tempdir().unwrap();
    let port = holon_fleet::fleet::free_port();
    let mut child = std::process::Command::new(holon_fleet::fleet::bin_path("comp-checks"))
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

    // Unseeded, the runner says so rather than guessing.
    let cold =
        compose::gate(&url, None, commit, &json!([]), &json!([]), &checks, Duration::from_secs(30));
    assert!(cold.is_ok(), "the runner did not answer at all: {cold:?}");
    assert!(!cold.unwrap().passed, "a cold runner cannot have passed the base check");

    compose::seed_base(&url, None, commit, &tree, Duration::from_secs(60))
        .expect("seeding the base");

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

    // An unseeded commit still asks, so the seed is doing the work.
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
