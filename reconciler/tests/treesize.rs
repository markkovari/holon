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
    let addr = fleet
        .nats_url
        .strip_prefix("nats://")
        .expect("a nats:// url")
        .to_string();

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
