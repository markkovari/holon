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
    auth_req(method, path, body, "")
}

fn auth_req(method: &str, path: &str, body: Option<Value>, token: &str) -> (u16, Value) {
    let url = format!("http://{ADDR}{path}");
    let mut r = ureq::request(method, &url);
    if !token.is_empty() {
        r = r.set("authorization", &format!("Bearer {token}"));
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
        .args([
            "--app", "binder", "--component", &composed, "--addr", ADDR,
            // auth-guard reads its tenant from config; without it every register
            // lands in a different tenant from every login.
            "--config", "default-tenant=binder",
        ])
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

    // --- the collection belongs to somebody ------------------------------
    //
    // Checked FIRST, because a route that forgets to introspect reads whichever
    // collection it finds and every assertion below would still pass.
    let (s, _) = req("GET", "/api/cards", None);
    assert_eq!(s, 401, "no token, no collection");
    let creds = json!({ "email": "mark@binder.test", "password": "pw12345678" });
    let (s, _) = req("POST", "/api/register", Some(creds.clone()));
    assert_eq!(s, 201, "register");
    let (s, session) = req("POST", "/api/login", Some(creds));
    assert_eq!(s, 200, "login: {session}");
    let token = session["access_token"].as_str().expect("a token").to_string();

    // --- the scan path: card:identify, through the composition -----------
    //
    // Fenced JSON with prose either side, which is how a model actually answers.
    let answer = "Looking at the photo:\n```json\n{\"name\":\"Charizard ex\",\
        \"set_name\":\"Obsidian Flames\",\"set_code\":\"SV3\",\"number\":\"125/197\",\
        \"rarity\":\"Double Rare\",\"language\":\"en\",\"variant\":\"holo\",\
        \"condition\":\"near mint\",\"confidence\":88}\n```\nHope that helps.";
    let (s, card) = auth_req("POST", "/api/scan", Some(json!({ "answer": answer })), &token);
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
    let (s, partial) = auth_req(
        "POST",
        "/api/scan",
        Some(json!({ "answer": r#"{"name":"Pikachu","set_name":"Base","set_code":"base1","confidence":40}"# })),
        &token,
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
    let (s, _) = auth_req("POST", "/api/scan", Some(json!({ "answer": r#"{"no_card":true,"reason":"a wrapper"}"# })), &token);
    assert_eq!(s, 422, "a photo that is not a card");
    let (s, _) = auth_req("POST", "/api/scan", Some(json!({ "answer": r#"{"cards_visible":2,"name":"Pikachu"}"# })), &token);
    assert_eq!(s, 422, "two cards in one photo");
    let (_, listed) = auth_req("GET", "/api/cards", None, &token);
    assert_eq!(listed["cards"].as_array().expect("array").len(), 2, "neither refusal was stored");

    // --- a correction clears the flag ------------------------------------
    let (s, fixed) = auth_req("PATCH", "/api/cards", Some(json!({ "id": commons, "condition": "lightly played" })), &token);
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
        let (s, v) = auth_req(
            "POST",
            "/api/events",
            Some(json!({ "card_id": charizard, "kind": kind, "quantity": qty,
                         "unit_minor": unit, "at": now - days_ago * DAY })),
        &token,
        );
        assert_eq!(s, 201, "{v}");
    }
    // Forty commons nothing will ever quote.
    auth_req("POST", "/api/events",
        Some(json!({ "card_id": commons, "kind": "acquired", "quantity": 40,
                     "unit_minor": 5, "at": now - 50 * DAY })), &token);

    for (days_ago, unit) in [(45u64, 4500i64), (30, 6000), (10, 9000)] {
        let (s, v) = auth_req("POST", "/api/quotes",
            Some(json!({ "card_id": charizard, "unit_minor": unit, "at": now - days_ago * DAY })), &token);
        assert_eq!(s, 201, "{v}");
    }

    let (s, p) = auth_req("GET", "/api/portfolio", None, &token);
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
    let (s, pr) = auth_req("GET", &format!("/api/price/{charizard}"), None, &token);
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

    // --- another account sees none of it ---------------------------------
    //
    // The assertion that makes "belongs to somebody" mean something. Every route
    // above could introspect correctly and still read one shared key space.
    let other = json!({ "email": "someone@binder.test", "password": "pw12345678" });
    req("POST", "/api/register", Some(other.clone()));
    let (_, s2) = req("POST", "/api/login", Some(other));
    let other_token = s2["access_token"].as_str().expect("a token").to_string();
    let (_, theirs) = auth_req("GET", "/api/cards", None, &other_token);
    assert!(
        theirs["cards"].as_array().expect("array").is_empty(),
        "a second account sees an empty collection, not this one: {theirs}"
    );
    let (_, their_p) = auth_req("GET", "/api/portfolio", None, &other_token);
    assert_eq!(their_p["cost_basis_minor"], 0, "and owns nothing: {their_p}");

    // --- a card typed in by hand -----------------------------------------
    //
    // Not a scan, so nothing is flagged and confidence is full — and the money is
    // recorded in the same action, because adding a card and saying what it cost is
    // one thing to a person.
    let (s, typed) = auth_req(
        "POST",
        "/api/cards",
        Some(json!({ "name": "Mew", "set_name": "Wizards Black Star", "set_code": "WBSP",
                     "number": "009", "condition": "near mint", "paid_minor": 4200, "quantity": 1 })),
        &token,
    );
    assert_eq!(s, 201, "{typed}");
    assert_eq!(typed["confidence"], 100, "a person who typed it is not guessing");
    assert!(typed["needs_review"].as_array().expect("array").is_empty(), "{typed}");
    assert_eq!(typed["set_code"], "wbsp", "still lowercased for lookup");

    let (_, after) = auth_req("GET", "/api/portfolio", None, &token);
    assert_eq!(
        after["cost_basis_minor"].as_i64().expect("i64"),
        p["cost_basis_minor"].as_i64().expect("i64") + 4200,
        "the price paid landed on the basis in the same action"
    );

    // A card can be removed, and its HISTORY is not rewritten by removing it.
    let (s, _) = auth_req("DELETE", "/api/cards", Some(json!({ "id": typed["id"] })), &token);
    assert_eq!(s, 200);
    let (_, still) = auth_req("GET", "/api/portfolio", None, &token);
    assert_eq!(
        still["cost_basis_minor"], after["cost_basis_minor"],
        "deleting the row must not silently rewrite what was paid"
    );

    // --- decks ------------------------------------------------------------
    //
    // Eight Charmander across TWO printings. Counting by the id the collection is
    // keyed on says four and four and passes; the rule counts names.
    let deck = json!({
        "name": "charizard",
        "slots": [
            { "card_id": "sv3-001",  "name": "Charmander",   "kind": "basic-pokemon",   "quantity": 4 },
            { "card_id": "base1-046","name": "Charmander",   "kind": "basic-pokemon",   "quantity": 4 },
            { "card_id": charizard,  "name": "Charizard ex", "kind": "evolved-pokemon", "quantity": 4 },
            { "card_id": "sve-002",  "name": "Fire Energy",  "kind": "basic-energy",    "quantity": 47 },
        ]
    });
    let (s, _) = auth_req("PUT", "/api/decks", Some(deck), &token);
    assert_eq!(s, 200, "save the deck");

    let (s, checked) = auth_req("GET", "/api/decks/charizard", None, &token);
    assert_eq!(s, 200, "{checked}");
    assert_eq!(checked["cards"], 59);
    assert_eq!(checked["legal"], false);
    let rules: Vec<&str> = checked["illegal"]
        .as_array()
        .expect("array")
        .iter()
        .map(|i| i["rule"].as_str().unwrap())
        .collect();
    assert!(rules.contains(&"size"), "59 is not 60: {rules:?}");
    assert!(rules.contains(&"four-copies"), "8 Charmander across two printings: {rules:?}");
    let four = checked["illegal"]
        .as_array()
        .expect("array")
        .iter()
        .find(|i| i["rule"] == "four-copies")
        .expect("the rule");
    assert!(
        four["detail"].as_str().expect("detail").contains("8 × Charmander"),
        "and it says how many: {four}"
    );

    // The shopping list is BY PRINTING even though the cap is by name, and the
    // cards nothing has priced are counted rather than quietly costed at zero.
    let missing = checked["missing"].as_array().expect("array");
    let charmanders: Vec<&Value> =
        missing.iter().filter(|m| m["name"] == "Charmander").collect();
    assert_eq!(charmanders.len(), 2, "two printings are two things to buy: {missing:?}");
    assert!(checked["unpriced"].as_u64().expect("u64") >= 3, "{checked}");
    // Two of the four Charizard are already owned, so only two are owing.
    let charizard_line =
        missing.iter().find(|m| m["card_id"] == json!(charizard)).expect("a line");
    assert_eq!(charizard_line["quantity"], 2, "the shortfall is what is NOT owned: {charizard_line}");

    // --- the series carries its numbers, not just a shape ------------------
    let point = &after["series"].as_array().expect("array")[0];
    for field in
        ["at", "market_value_minor", "cost_basis_minor", "realised_minor", "unrealised_minor", "unquoted"]
    {
        assert!(point.get(field).is_some(), "a hoverable point needs {field}: {point}");
    }

    // --- deck CRUD, and a name with a space in it -------------------------
    //
    // "some deck" is stored decoded and asked for as `some%20deck`. Without a decode
    // on the path the two never meet: creating it works and opening it is a 404,
    // with the name looking correct in both places.
    let (s, _) = auth_req("POST", "/api/decks", Some(json!({ "name": "some deck" })), &token);
    assert_eq!(s, 201, "create");
    let (s, _) = auth_req("POST", "/api/decks", Some(json!({ "name": "some deck" })), &token);
    assert_eq!(s, 409, "a second deck by the same name is refused");

    let (s, filled) = auth_req(
        "POST",
        "/api/decks/some%20deck/slots",
        Some(json!({ "card_id": charizard, "quantity": 4, "kind": "evolved-pokemon" })),
        &token,
    );
    assert_eq!(s, 200, "{filled}");
    let (s, opened) = auth_req("GET", "/api/decks/some%20deck", None, &token);
    assert_eq!(s, 200, "a name with a space opens by name: {opened}");
    assert_eq!(opened["cards"], 4);

    // A card is in as many decks as you like, and being in one does not take it out
    // of the collection.
    auth_req("POST", "/api/decks", Some(json!({ "name": "second" })), &token);
    auth_req(
        "POST",
        "/api/decks/second/slots",
        Some(json!({ "card_id": charizard, "quantity": 2, "kind": "evolved-pokemon" })),
        &token,
    );
    let (_, listed) = auth_req("GET", "/api/cards", None, &token);
    let card = listed["cards"]
        .as_array()
        .expect("array")
        .iter()
        .find(|c| c["id"] == json!(charizard))
        .expect("the card");
    let in_decks: Vec<&str> =
        card["in_decks"].as_array().expect("array").iter().map(|d| d.as_str().unwrap()).collect();
    assert!(in_decks.contains(&"second") && in_decks.contains(&"some deck"), "{in_decks:?}");

    // Zero removes the slot — one route for "how many" and "is it in", so the two
    // cannot disagree.
    let (s, emptied) = auth_req(
        "POST",
        "/api/decks/second/slots",
        Some(json!({ "card_id": charizard, "quantity": 0 })),
        &token,
    );
    assert_eq!(s, 200);
    assert!(emptied["slots"].as_array().expect("array").is_empty(), "{emptied}");

    // Deleting a deck deletes the LIST. The cards it named are still owned.
    let owned_before = listed["cards"].as_array().expect("array").len();
    let (s, _) = auth_req("DELETE", "/api/decks", Some(json!({ "name": "second" })), &token);
    assert_eq!(s, 200);
    let (_, after_delete) = auth_req("GET", "/api/cards", None, &token);
    assert_eq!(
        after_delete["cards"].as_array().expect("array").len(),
        owned_before,
        "a deck is a list, not a box"
    );
    let (_, decks) = auth_req("GET", "/api/decks", None, &token);
    let names: Vec<&str> =
        decks["decks"].as_array().expect("array").iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert!(!names.contains(&"second"), "the deleted one is gone: {names:?}");
    assert!(names.contains(&"some deck"), "and the others are not: {names:?}");

    // --- the photo path is async ------------------------------------------
    //
    // The upload STORES and answers; the vision call happens on the event stream.
    // Asserted without a model: the upload must be immediate and must validate what
    // it can, because those are the parts that must not wait for a provider.
    let (s, bad) = auth_req("POST", "/api/photo", Some(json!({ "media_type": "image/png" })), &token);
    assert_eq!(s, 400, "no image is the caller's mistake, on the request that made it: {bad}");
    let (s, bad) = auth_req(
        "POST",
        "/api/photo",
        Some(json!({ "media_type": "image/png", "data": "not base64!!" })),
        &token,
    );
    assert_eq!(s, 400, "and so is a payload that is not base64: {bad}");

    // A 1x1 PNG, so the upload has something real to store.
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let (s, job) = auth_req(
        "POST",
        "/api/photo",
        Some(json!({ "media_type": "image/png", "data": png })),
        &token,
    );
    assert_eq!(s, 202, "accepted, not done: {job}");
    assert!(job["job"].is_string(), "{job}");
    assert_eq!(
        job["events"].as_str().expect("a stream url"),
        format!("/api/photo/{}/events", job["job"].as_str().unwrap()),
        "and it says where to watch"
    );

    // The stream is the only thing that can spend a vision call, so a job is claimed
    // once: a reconnect must not pay twice for one picture.
    let stream = job["events"].as_str().expect("url").to_string();
    let (s, _) = auth_req("GET", &stream, None, &token);
    // Either it streamed (200) or the provider was unreachable — both are fine here;
    // what matters is the SECOND read.
    assert!(s == 200 || s == 502, "the first read runs the job: {s}");
    let (s, gone) = auth_req("GET", &stream, None, &token);
    assert_eq!(s, 404, "the job was claimed and is not there twice: {gone}");
}
