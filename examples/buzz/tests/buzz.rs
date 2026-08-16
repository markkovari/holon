//! E2E for the buzz live quiz game (docs/apps/BUZZ.md) as ONE composed wasm HTTP component
//! (buzz-domain + auth-guard + records) on the native Rust host. Proves the game
//! loop and SPEED-WEIGHTED scoring: a host runs a game; players join by PIN and
//! answer; on reveal a faster correct answer beats a slower correct one, a wrong
//! answer scores zero, the leaderboard ranks by score, and the game ends on a
//! podium. Also checks the gates: no join after start, one answer per question,
//! and host-only controls.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3049";

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

fn req(method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, Value) {
    let url = format!("{}{}", base(), path);
    let mut r = ureq::request(method, &url);
    if let Some(t) = token {
        r = r.set("authorization", &format!("Bearer {t}"));
    }
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

fn signup(email: &str) -> String {
    let (s, _) = req("POST", "/api/register", None, Some(json!({ "email": email, "password": "pw12345678" })));
    assert!(s == 201 || s == 409, "register {email}: {s}");
    let (s, l) = req("POST", "/api/login", None, Some(json!({ "email": email, "password": "pw12345678" })));
    assert_eq!(s, 200, "login {email}: {l}");
    l["access_token"].as_str().unwrap().to_string()
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/buzz_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-buzz`)");
    assert!(component.exists(), "composed wasm missing (just compose-buzz)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "buzz")
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
    panic!("buzz host did not start");
}

fn join(pin: &str, nick: &str) -> String {
    let (s, r) = req("POST", &format!("/api/games/{pin}/join"), None, Some(json!({ "nickname": nick })));
    assert_eq!(s, 201, "join {nick}: {r}");
    r["player"].as_str().unwrap().to_string()
}
fn answer(pin: &str, player: &str, option: u64) {
    let (s, _) = req("POST", &format!("/api/games/{pin}/answer"), None, Some(json!({ "player": player, "option": option })));
    assert_eq!(s, 200);
}

#[test]
fn live_game_speed_weighted_scoring() {
    let _host = start_host();
    let host = signup("host@acme.io"); // registering seeds a demo quiz ("WIT Warm-up")

    // ===== host starts a game -> a PIN ====================================
    let (_, q) = req("GET", "/api/quizzes", Some(&host), None);
    let quiz = q["items"][0]["id"].as_str().unwrap().to_string();
    let (s, g) = req("POST", "/api/games", Some(&host), Some(json!({ "quiz": quiz })));
    assert_eq!(s, 201, "{g}");
    let pin = g["pin"].as_str().unwrap().to_string();
    assert_eq!(pin.len(), 6, "6-digit PIN");

    // three players join the lobby.
    let ada = join(&pin, "Ada");
    let bo = join(&pin, "Bo");
    let cy = join(&pin, "Cy");
    let (_, hv) = req("GET", &format!("/api/games/{pin}/host"), Some(&host), None);
    assert_eq!(hv["players"].as_array().unwrap().len(), 3);

    // ===== gates: only the host drives; players answer only in a question ==
    assert_eq!(req("POST", &format!("/api/games/{pin}/start"), None, None).0, 401, "player can't start (no token)");
    assert_eq!(req("POST", &format!("/api/games/{pin}/answer"), None, Some(json!({ "player": ada, "option": 1 }))).0, 409, "no answers before start");

    // ===== start Q1: answer key is option index 1 ==========================
    assert_eq!(req("POST", &format!("/api/games/{pin}/start"), Some(&host), None).0, 200);
    // no more joining once started.
    assert_eq!(req("POST", &format!("/api/games/{pin}/join"), None, Some(json!({ "nickname": "Late" }))).0, 409);

    // Ada answers correctly, immediately; Bo answers correctly but ~1.2s later;
    // Cy answers wrong. Faster-correct must beat slower-correct; wrong = 0.
    answer(&pin, &ada, 1);
    std::thread::sleep(Duration::from_millis(1200));
    answer(&pin, &bo, 1);
    answer(&pin, &cy, 0);
    // one answer per question is enforced.
    answer(&pin, &ada, 2); // ignored (already answered)

    let (_, hv) = req("GET", &format!("/api/games/{pin}/host"), Some(&host), None);
    assert_eq!(hv["answered"], 3);
    assert_eq!(hv["counts"], json!([1, 2, 0, 0]), "one wrong, two correct");

    // ===== reveal grades speed-weighted ===================================
    assert_eq!(req("POST", &format!("/api/games/{pin}/reveal"), Some(&host), None).0, 200);
    let score = |pid: &str| -> i64 {
        let (_, pv) = req("GET", &format!("/api/games/{pin}/play?player={pid}"), None, None);
        pv["my_score"].as_i64().unwrap()
    };
    let (sa, sb, sc) = (score(&ada), score(&bo), score(&cy));
    assert!(sa > sb, "faster correct ({sa}) beats slower correct ({sb})");
    assert!(sb > 0, "slower-but-correct still scores ({sb})");
    assert_eq!(sc, 0, "wrong answer scores zero");
    assert!(sa <= 1000, "capped at the base ({sa})");

    // leaderboard ranks by score.
    let (_, hv) = req("GET", &format!("/api/games/{pin}/host"), Some(&host), None);
    let board: Vec<&str> = hv["leaderboard"].as_array().unwrap().iter().map(|p| p["nickname"].as_str().unwrap()).collect();
    assert_eq!(board[0], "Ada", "Ada leads: {board:?}");
    // a student's own rank matches.
    let (_, pv) = req("GET", &format!("/api/games/{pin}/play?player={ada}"), None, None);
    assert_eq!(pv["my_rank"], 1);

    // ===== play out the rest, then a final podium =========================
    for _ in 0..2 {
        req("POST", &format!("/api/games/{pin}/next"), Some(&host), None);
        // everyone answers correctly this time.
        for p in [&ada, &bo, &cy] {
            answer(&pin, p, 0); // Q2 answer key = 0
        }
        req("POST", &format!("/api/games/{pin}/reveal"), Some(&host), None);
    }
    // after revealing the last question, one more `next` ends the game.
    req("POST", &format!("/api/games/{pin}/next"), Some(&host), None);
    let (_, hv) = req("GET", &format!("/api/games/{pin}/host"), Some(&host), None);
    assert_eq!(hv["phase"], "final", "game ends after the last question");
    let (_, pv) = req("GET", &format!("/api/games/{pin}/play?player={ada}"), None, None);
    assert_eq!(pv["podium"].as_array().unwrap().len().min(3), 3.min(3), "a podium of the top players");
    assert!(pv["podium"][0]["score"].as_i64().unwrap() >= pv["podium"][1]["score"].as_i64().unwrap(), "podium sorted");
}
