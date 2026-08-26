//! E2E for the card binder (docs/apps/BINDER.md) as ONE composed wasm HTTP component
//! on the native host.
//!
//! What this proves that the capability suites cannot: that the three of them are
//! actually WIRED. `card:identify`, `price:history` and `portfolio:value` each pass a
//! held-out specification in isolation, and a composition can still hand the wrong
//! field to the right function — so every assertion below is on a NUMBER that has
//! travelled through the linker, not on a status code.
//!
//! The arithmetic is chosen so a plausible wrong implementation fails:
//!
//!   buy 2 @ €10.00, buy 1 @ €40.00, sell 1 @ €30.00
//!
//! FIFO realises €20.00 and leaves €50.00 of basis. Average cost realises €10.00 and
//! leaves €40.00. Both are "a number on a chart"; only one is right, and the app is
//! not allowed to be the thing that decides which.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3211";
const DAY: u64 = 86_400;

struct HostGuard(Child);
impl Drop for HostGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn req(method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let url = format!("http://{ADDR}{path}");
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

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

fn start_host() -> HostGuard {
    // Refuse to run against a host this test did not start. A leaked `comp-host`
    // from an interrupted run keeps the port AND its in-memory collection, so the
    // events below land on top of an earlier run's and every total comes out a
    // multiple of the right answer — which reads as broken arithmetic in the
    // capability rather than as a stale process.
    match std::net::TcpListener::bind(ADDR) {
        Ok(l) => drop(l),
        Err(e) => panic!(
            "something is already listening on {ADDR} ({e}). A comp-host from an \
             earlier run is still up and its store is not empty — `pkill -f comp-host`"
        ),
    }

    let root = repo_root();
    let bin = root.join("host/target/release/comp-host");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-binder`)");

    // The DERIVED composition (ADR-0087), asked for by name rather than by a path a
    // recipe had to keep in step with the digest.
    let plug = root.join("reconciler/target/release/comp-plug");
    assert!(plug.exists(), "comp-plug not built (run `just e2e-binder`)");
    // From the repo root: `comp-plug` resolves a component by name against
    // `components/`, and the test's own cwd is this crate.
    let composed =
        Command::new(&plug).arg("binder-domain").current_dir(&root).output().expect("comp-plug");
    let composed = String::from_utf8_lossy(&composed.stdout).trim().to_string();
    assert!(!composed.is_empty(), "comp-plug produced no artifact — is binder-domain built?");

    let mut child = Command::new(&bin)
        .args(["--app", "binder", "--component", &composed, "--addr", ADDR])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("comp-host");

    // Wait for the line the host prints when it is actually serving, rather than
    // sleeping and hoping: a fixed sleep is the difference between a suite that is
    // flaky on a loaded machine and one that is not.
    // BOTH streams: the host writes its banner to stdout, and watching only stderr
    // is a 30-second timeout that looks exactly like a host that failed to start.
    let (tx, rx) = std::sync::mpsc::channel();
    for stream in [
        Box::new(child.stdout.take().expect("stdout")) as Box<dyn std::io::Read + Send>,
        Box::new(child.stderr.take().expect("stderr")),
    ] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if line.contains("serving") {
                    let _ = tx.send(());
                }
            }
        });
    }
    rx.recv_timeout(Duration::from_secs(30)).expect("the host never reported serving");
    HostGuard(child)
}

/// One test, because the state is one collection and splitting it would need either a
/// fixture per test or an order dependency between them.
#[test]
fn a_photographed_collection_prices_itself() {
    let _host = start_host();
    // The app values the collection as of ITS clock, and events after that instant
    // are ignored by design — so a fixed timestamp in the future makes every number
    // zero and reads like broken arithmetic. Anchor on the same wall clock.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs();

    // --- the scan path: card:identify, through the composition -----------
    //
    // Fenced JSON with prose either side, which is how a model actually answers.
    let answer = "Looking at the photo:\n```json\n{\"name\":\"Charizard ex\",\
        \"set_name\":\"Obsidian Flames\",\"set_code\":\"SV3\",\"number\":\"125/197\",\
        \"rarity\":\"Double Rare\",\"language\":\"en\",\"variant\":\"holo\",\
        \"condition\":\"near mint\",\"confidence\":88}\n```\nHope that helps.";
    let (s, card) = req("POST", "/api/scan", Some(json!({ "answer": answer })));
    assert_eq!(s, 201, "{card}");
    assert_eq!(card["name"], "Charizard ex");
    assert_eq!(card["set_code"], "sv3", "lowercased for lookup");
    assert_eq!(card["printing"], "holo", "the contract's word, not Rust's Debug");
    assert_eq!(card["condition"], "near mint");
    assert!(
        card["needs_review"].as_array().expect("array").is_empty(),
        "a complete answer leaves nothing to check: {card}"
    );
    let charizard = card["id"].as_str().expect("an id").to_string();

    // A partial answer must NOT be completed with defaults, and must say what is
    // missing. This is the assertion that a silently-defaulted condition fails.
    let (s, partial) = req(
        "POST",
        "/api/scan",
        Some(json!({ "answer": r#"{"name":"Pikachu","set_name":"Base","set_code":"base1","confidence":40}"# })),
    );
    assert_eq!(s, 201, "{partial}");
    assert_eq!(partial["condition"], "", "NOT defaulted to near mint");
    let review: Vec<&str> =
        partial["needs_review"].as_array().expect("array").iter().map(|v| v.as_str().unwrap()).collect();
    for field in ["condition", "printing", "number", "rarity", "language"] {
        assert!(review.contains(&field), "{field} is absent and must be flagged: {review:?}");
    }
    let commons = partial["id"].as_str().expect("an id").to_string();

    // Refusals reach the caller as refusals rather than as blank rows.
    let (s, _) = req("POST", "/api/scan", Some(json!({ "answer": r#"{"no_card":true,"reason":"a wrapper"}"# })));
    assert_eq!(s, 422, "a photo that is not a card");
    let (s, _) = req("POST", "/api/scan", Some(json!({ "answer": r#"{"cards_visible":2,"name":"Pikachu"}"# })));
    assert_eq!(s, 422, "two cards in one photo");
    let (_, listed) = req("GET", "/api/cards", None);
    assert_eq!(listed["cards"].as_array().expect("array").len(), 2, "neither refusal was stored");

    // --- a correction clears the flag ------------------------------------
    let (s, fixed) = req("PATCH", "/api/cards", Some(json!({ "id": commons, "condition": "lightly played" })));
    assert_eq!(s, 200, "{fixed}");
    assert_eq!(fixed["condition"], "lightly played");
    let still: Vec<&str> =
        fixed["needs_review"].as_array().expect("array").iter().map(|v| v.as_str().unwrap()).collect();
    assert!(!still.contains(&"condition"), "a checked field stops being flagged: {still:?}");
    assert!(still.contains(&"number"), "and the others do not: {still:?}");

    // --- the money: portfolio:value, through the composition -------------
    for (kind, qty, unit, days_ago) in [
        ("acquired", 2u32, 1000i64, 60u64),
        ("acquired", 1, 4000, 40),
        ("disposed", 1, 3000, 20),
    ] {
        let (s, v) = req(
            "POST",
            "/api/events",
            Some(json!({ "card_id": charizard, "kind": kind, "quantity": qty,
                         "unit_minor": unit, "at": now - days_ago * DAY })),
        );
        assert_eq!(s, 201, "{v}");
    }
    // Forty commons nothing will ever quote.
    req("POST", "/api/events",
        Some(json!({ "card_id": commons, "kind": "acquired", "quantity": 40,
                     "unit_minor": 5, "at": now - 50 * DAY })));

    for (days_ago, unit) in [(45u64, 4500i64), (30, 6000), (10, 9000)] {
        let (s, v) = req("POST", "/api/quotes",
            Some(json!({ "card_id": charizard, "unit_minor": unit, "at": now - days_ago * DAY })));
        assert_eq!(s, 201, "{v}");
    }

    let (s, p) = req("GET", "/api/portfolio", None);
    assert_eq!(s, 200, "{p}");

    // FIFO: the copy that left cost €10.00 and sold for €30.00.
    assert_eq!(p["realised_minor"], 2000, "average cost would say 1000: {p}");
    // One €10.00 lot and one €40.00 lot still held, plus 40 commons at 5.
    assert_eq!(p["cost_basis_minor"], 5000 + 200, "{p}");
    // Two Charizard at the newest quote (€90.00), and the commons AT COST — not at
    // zero, which would make the chart dip, and not omitted, which would make it
    // climb.
    assert_eq!(p["market_value_minor"], 18_000 + 200, "{p}");
    assert_eq!(p["unquoted"], 40, "the commons are counted, not hidden: {p}");
    assert_eq!(p["unrealised_minor"], 18_200 - 5_200, "{p}");
    assert_eq!(p["currency"], "EUR");
    assert!(p["series"].as_array().expect("array").len() > 80, "90 days of samples: {p}");

    // --- the price series: price:history, through the composition --------
    let (s, pr) = req("GET", &format!("/api/price/{charizard}"), None);
    assert_eq!(s, 200, "{pr}");
    let points = pr["points"].as_array().expect("array");
    let carried = points.iter().filter(|p| p["carried"] == json!(true)).count();
    assert!(points.len() > 40, "the window is sampled: {}", points.len());
    assert!(carried > 0, "the days between quotes are CARRIED, not interpolated");
    assert!(
        points.iter().all(|p| [4500i64, 6000, 9000].contains(&p["unit_minor"].as_i64().unwrap())),
        "every value is an observed quote — an interpolated point would be none of the three"
    );
    // And nothing before the first quote: those samples are absent, not zero.
    assert!(
        points.iter().all(|p| p["unit_minor"].as_i64().unwrap() > 0),
        "a zero would be a price nobody ever saw"
    );
}
