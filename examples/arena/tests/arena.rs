//! E2E for the arena game (ARENA.md) as ONE composed wasm HTTP component
//! (arena-domain + records + id-generate) on the native Rust host. Proves
//! authoritative, rule-enforced interactive state: create + join, turn/seat and
//! illegal-move rejection server-side, a scripted win with the winning line
//! detected, no moves after the game ends, and a live SSE spectator seeing a
//! move.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3039";

struct HostGuard(Child);
impl Drop for HostGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn base() -> String {
    format!("http://{ADDR}")
}

fn req(method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let url = format!("{}{}", base(), path);
    let r = ureq::request(method, &url);
    let result = match &body {
        Some(b) => r.set("content-type", "application/json").send_string(&b.to_string()),
        None => r.call(),
    };
    let resp = match result {
        Ok(resp) => resp,
        Err(ureq::Error::Status(_, resp)) => resp,
        Err(e) => panic!("{method} {path}: {e}"),
    };
    let status = resp.status();
    (status, serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null))
}

fn mv(id: &str, token: &str, col: u64) -> (u16, Value) {
    req("POST", &format!("/api/games/{id}/move"), Some(json!({ "token": token, "col": col })))
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/arena_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-arena`)");
    assert!(component.exists(), "composed wasm missing (just compose-arena)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "arena")
        .spawn()
        .expect("spawn comp-host");
    let guard = HostGuard(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&base()).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("arena host did not start");
}

#[test]
fn full_game_with_rules_and_live_spectator() {
    let _host = start_host();

    // ===== create + join =====================================================
    let (s, c) = req("POST", "/api/games", Some(json!({ "name": "Ada" })));
    assert_eq!(s, 201, "create: {c}");
    let id = c["game"]["id"].as_str().unwrap().to_string();
    let red = c["token"].as_str().unwrap().to_string();
    assert_eq!(c["game"]["status"], "waiting");

    let (s, j) = req("POST", &format!("/api/games/{id}/join"), Some(json!({ "name": "Bob" })));
    assert_eq!(s, 200, "join: {j}");
    let yellow = j["token"].as_str().unwrap().to_string();
    assert_eq!(j["game"]["status"], "active");
    assert_eq!(j["game"]["turn"], "R", "red moves first");

    // ===== rule enforcement ==================================================
    // yellow can't move on red's turn
    let (s, _) = mv(&id, &yellow, 0);
    assert_eq!(s, 403, "out-of-turn move rejected");
    // a stranger's token is refused
    let (s, _) = mv(&id, "not-a-real-token", 0);
    assert_eq!(s, 403, "non-player rejected");
    // illegal column
    let (s, _) = mv(&id, &red, 99);
    assert_eq!(s, 422, "illegal column rejected");

    // ===== a scripted red win: R stacks column 0, Y stacks column 1 ==========
    // R,Y,R,Y,R,Y,R -> red gets four vertically in column 0.
    for i in 0..7 {
        let (tok, col) = if i % 2 == 0 { (&red, 0) } else { (&yellow, 1) };
        let (s, g) = mv(&id, tok, col);
        assert_eq!(s, 200, "move {i}: {g}");
        if i < 6 {
            // double-move by the same player is rejected (turn already flipped)
            let (s2, _) = mv(&id, tok, col);
            assert_eq!(s2, 403, "double move rejected at {i}");
        }
    }
    let (_, g) = req("GET", &format!("/api/games/{id}"), None);
    assert_eq!(g["status"], "finished", "game over: {g}");
    assert_eq!(g["winner"], "R", "red won: {g}");
    assert_eq!(g["line"].as_array().unwrap().len(), 4, "winning line of four: {g}");

    // no moves after the game ends
    let (s, _) = mv(&id, &yellow, 2);
    assert_eq!(s, 409, "no moves after finish");

    // ===== live SSE spectator sees a move ====================================
    let (_, c2) = req("POST", "/api/games", Some(json!({ "name": "Cy" })));
    let id2 = c2["game"]["id"].as_str().unwrap().to_string();
    let red2 = c2["token"].as_str().unwrap().to_string();
    req("POST", &format!("/api/games/{id2}/join"), Some(json!({ "name": "Di" })));

    let found = Arc::new(AtomicBool::new(false));
    let f = found.clone();
    let url = format!("{}/api/games/{id2}/events", base());
    let reader = std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new().timeout_read(Duration::from_secs(2)).build();
        let Ok(resp) = agent.get(&url).call() else { return };
        let mut buf = BufReader::new(resp.into_reader());
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut line = String::new();
        while Instant::now() < deadline {
            line.clear();
            match buf.read_line(&mut line) {
                Ok(0) => break,
                // a move puts a disc in the board; the empty board is all dots.
                Ok(_) => {
                    if line.starts_with("data:") && line.contains("\"turn\":\"Y\"") {
                        f.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                Err(_) => {}
            }
        }
    });
    std::thread::sleep(Duration::from_millis(700));
    let (s, _) = mv(&id2, &red2, 3); // red moves -> turn flips to Y, spectator sees it
    assert_eq!(s, 200);
    reader.join().unwrap();
    assert!(found.load(Ordering::SeqCst), "spectator should see the move over SSE");
}
