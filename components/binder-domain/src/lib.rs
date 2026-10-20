//! Pokémon card collection. Every key is per-account (`u/<subject>/`) except quotes, a market fact.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../audit-log/wit",
            "../../wit/deps/ratelimit-guard",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../zip/wit",
            "../csv/wit",
            "../sheet-ingest/wit",
            "../qr/wit",
            "../id-generate/wit",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../deck-build/wit",
            "../vision-describe/wit",
            "../card-identify/wit",
            "../price-history/wit",
            "../portfolio-value/wit",
            "wit",
        ],
        world: "binder:app/binder-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
        pub use super::wasi::random0_3_0_rc_2026_03_15 as random;
    }
}

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::types::Principal;
use bindings::card::identify::identifier as ident;
use bindings::deck::build::builder as deck;
use bindings::id::generate::generator as ids;
use bindings::p3::clocks::system_clock;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};
use bindings::portfolio::value::valuation as pv;
use bindings::price::history::history as ph;
use bindings::qr::encode::encoder as qr;
use bindings::sheet::ingest::reader as sheet;
use bindings::vision::describe::describer as vision;
use bindings::wasi::keyvalue::store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

struct Component;

// The host names the bucket after the app (ADR-0023); this string is not the isolation boundary.
const BUCKET: &str = "default";

#[derive(Serialize, Deserialize, Clone)]
struct Card {
    id: String,
    name: String,
    #[serde(default)]
    set_name: String,
    #[serde(default)]
    set_code: String,
    #[serde(default)]
    number: String,
    #[serde(default)]
    rarity: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    printing: String,
    #[serde(default)]
    condition: String,
    #[serde(default)]
    graded: String,
    #[serde(default)]
    confidence: u8,
    #[serde(default)]
    needs_review: Vec<String>,
}

/// Both sides are valued at `value_minor`, so the better end of a trade shows up as a gain.
#[derive(Serialize, Deserialize, Clone)]
struct Swap {
    id: String,
    from: String,
    give: String,
    want: String,
    value_minor: i64,
    currency: String,
    /// Empty while open.
    taken_by: String,
    at: u64,
}

#[derive(Serialize, Deserialize, Clone)]
struct StoredEvent {
    card_id: String,
    kind: String,
    quantity: u32,
    unit_minor: i64,
    currency: String,
    at: u64,
}

/// Appended per change, so who-changed-what survives the card overwrite.
#[derive(Serialize, Deserialize, Clone)]
struct Change {
    card_id: String,
    field: String,
    from: String,
    to: String,
    at: u64,
}

#[derive(Serialize, Deserialize, Clone)]
struct StoredQuote {
    card_id: String,
    unit_minor: i64,
    currency: String,
    at: u64,
}

/// The contract's own word for a printing, not Rust's `Debug`.
fn printing_name(p: ident::VariantKind) -> String {
    match p {
        ident::VariantKind::Normal => "normal",
        ident::VariantKind::Holo => "holo",
        ident::VariantKind::ReverseHolo => "reverse holo",
        ident::VariantKind::FirstEdition => "1st edition",
        ident::VariantKind::Shadowless => "shadowless",
        ident::VariantKind::Special => "special",
    }
    .to_string()
}

fn condition_name(c: ident::Condition) -> String {
    match c {
        ident::Condition::Mint => "mint",
        ident::Condition::NearMint => "near mint",
        ident::Condition::LightlyPlayed => "lightly played",
        ident::Condition::ModeratelyPlayed => "moderately played",
        ident::Condition::HeavilyPlayed => "heavily played",
        ident::Condition::Damaged => "damaged",
    }
    .to_string()
}

fn store_guess(b: &store::Bucket, ns: &str, g: ident::Guess) -> Result<Card, String> {
    let id = format!(
        "{}-{}",
        if g.set_code.is_empty() { "unknown" } else { &g.set_code },
        if g.number.is_empty() {
            g.name.replace(' ', "-").to_lowercase()
        } else {
            g.number.replace('/', "-")
        }
    );
    let card = Card {
        id: id.clone(),
        name: g.name,
        set_name: g.set_name,
        set_code: g.set_code,
        number: g.number,
        rarity: g.rarity,
        language: g.language,
        printing: g.printing.map(printing_name).unwrap_or_default(),
        condition: g.condition.map(condition_name).unwrap_or_default(),
        graded: g
            .graded
            .map(|gr| format!("{} {}", gr.grader, gr.tenths as f64 / 10.0))
            .unwrap_or_default(),
        confidence: g.confidence,
        // `variant` is a WIT keyword, so the contract calls it `printing`.
        needs_review: g
            .needs_review
            .into_iter()
            .map(|f| if f == "variant" { "printing".to_string() } else { f })
            .collect(),
    };
    put_json(b, &format!("{ns}card:{id}"), &card)?;
    Ok(card)
}

/// Hand-rolled for one alphabet; whitespace is skipped because a data URL may arrive wrapped.
fn decode_b64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(v))
}

use guestfmt::percent_decode;

fn now() -> u64 {
    system_clock::now().seconds as u64
}

fn open() -> Result<store::Bucket, String> {
    store::open(BUCKET).map_err(|e| format!("{e:?}"))
}

fn get_json<T: for<'de> Deserialize<'de>>(b: &store::Bucket, key: &str) -> Option<T> {
    b.get(key).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok())
}

fn put_json<T: Serialize>(b: &store::Bucket, key: &str, v: &T) -> Result<(), String> {
    b.set(key, &serde_json::to_vec(v).map_err(|e| e.to_string())?).map_err(|e| format!("{e:?}"))
}

/// Prefix scan, no index: a collection is small and an index can drift.
fn scan<T: for<'de> Deserialize<'de>>(b: &store::Bucket, prefix: &str) -> Vec<T> {
    // A set, stopping when a page adds nothing: this host's draft `list-keys` returns the
    // whole key space with a cursor, so looping to a none cursor read every key three times.
    let mut keys = std::collections::BTreeSet::new();
    let mut cursor = None;
    while let Ok(page) = b.list_keys(cursor) {
        let before = keys.len();
        keys.extend(page.keys.into_iter().filter(|k| k.starts_with(prefix)));
        match page.cursor {
            Some(c) if keys.len() > before => cursor = Some(c),
            _ => break,
        }
    }
    keys.iter().filter_map(|k| get_json(b, k)).collect()
}

/// Everything that distinguishes an event is in the key, so a buy and a sell in one second
/// do not collide; a replayed POST still lands on the same key.
fn event_key(ns: &str, e: &StoredEvent) -> String {
    format!("{ns}event:{:020}:{}:{}:{}:{}", e.at, e.card_id, e.kind, e.quantity, e.unit_minor)
}

fn keys_under(b: &store::Bucket, prefix: &str) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut cursor = None;
    while let Ok(page) = b.list_keys(cursor) {
        let before = out.len();
        out.extend(page.keys.into_iter().filter(|k| k.starts_with(prefix)));
        match page.cursor {
            Some(c) if out.len() > before => cursor = Some(c),
            _ => break,
        }
    }
    out.into_iter().collect()
}

fn events_for(b: &store::Bucket, ns: &str) -> Vec<pv::Event> {
    scan::<StoredEvent>(b, &format!("{ns}event:"))
        .into_iter()
        .map(|e| pv::Event {
            item_id: e.card_id,
            kind: if e.kind == "disposed" {
                pv::EventKind::Disposed
            } else {
                pv::EventKind::Acquired
            },
            quantity: e.quantity,
            unit_minor: e.unit_minor,
            currency: e.currency,
            at: e.at,
        })
        .collect()
}

fn quotes_for(b: &store::Bucket) -> Vec<pv::Quote> {
    scan::<StoredQuote>(b, "quote:")
        .into_iter()
        .map(|q| pv::Quote {
            item_id: q.card_id,
            unit_minor: q.unit_minor,
            currency: q.currency,
            at: q.at,
        })
        .collect()
}

#[derive(Serialize, Deserialize, Clone)]
struct Deck {
    name: String,
    #[serde(default)]
    slots: Vec<DeckSlot>,
}

#[derive(Serialize, Deserialize, Clone)]
struct DeckSlot {
    card_id: String,
    name: String,
    kind: String,
    quantity: u32,
}

fn kind_of(s: &str) -> deck::CardKind {
    match s {
        "evolved-pokemon" => deck::CardKind::EvolvedPokemon,
        "trainer" => deck::CardKind::Trainer,
        "basic-energy" => deck::CardKind::BasicEnergy,
        "special-energy" => deck::CardKind::SpecialEnergy,
        // The only conservative default: capped at four and counts as a basic, so a mislabelled
        // card cannot make an illegal deck look legal.
        _ => deck::CardKind::BasicPokemon,
    }
}

fn kind_name(k: deck::CardKind) -> &'static str {
    match k {
        deck::CardKind::BasicPokemon => "basic-pokemon",
        deck::CardKind::EvolvedPokemon => "evolved-pokemon",
        deck::CardKind::Trainer => "trainer",
        deck::CardKind::BasicEnergy => "basic-energy",
        deck::CardKind::SpecialEnergy => "special-energy",
    }
}

/// One JSON event per stage with a `stage` field, so a client needs no prose parsing.
fn stream_photo(
    bucket: store::Bucket,
    ns: String,
    bytes: Vec<u8>,
    media_type: String,
) -> Result<Response, ErrorCode> {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"text/event-stream".to_vec()]);
    let _ = headers.set("cache-control", &[b"no-cache".to_vec()]);
    let (trailers_tx, trailers_rx) = bindings::wit_future::new(|| Ok(None));
    drop(trailers_tx);
    let (mut tx, rx) = bindings::wit_stream::new();
    // Runs after the response returns, so the first event arrives while the model is looking.
    wit_bindgen::spawn_local(async move {
        let bucket = &bucket;
        let ns = ns.as_str();
        // A p3 `stream<u8>` write has no 4096-byte trap; `write_all` loops and returns the rest
        // if the reader went away.
        let mut send = async |v: Value| -> bool {
            tx.write_all(format!("data: {v}\n\n").into_bytes()).await.is_empty()
        };

        if !send(json!({ "stage": "looking", "detail": "showing the card to the model" })).await {
            return;
        }

        match vision::describe(&bytes, &media_type, &ident::prompt()) {
            Ok(answer) => {
                let _ =
                    send(json!({ "stage": "reading", "detail": "reading the answer into fields" }))
                        .await;
                match ident::parse(&answer) {
                    Ok(g) => match store_guess(bucket, ns, g) {
                        Ok(card) => {
                            let _ = send(json!({
                                "stage": "done",
                                "card": serde_json::to_value(&card).unwrap_or(Value::Null),
                            }))
                            .await;
                        }
                        Err(e) => {
                            let _ = send(json!({ "stage": "failed", "error": e })).await;
                        }
                    },
                    Err(e) => {
                        let _ = send(json!({
                            "stage": "refused",
                            "error": format!("{e:?}"),
                            "said": answer.chars().take(400).collect::<String>(),
                        }))
                        .await;
                    }
                }
            }
            Err(e) => {
                let _ = send(json!({ "stage": "failed", "error": format!("{e:?}") })).await;
            }
        }
    });
    let (response, _sent) = Response::new(headers, Some(rx), trailers_rx);
    response
        .set_status_code(200)
        .map_err(|()| ErrorCode::InternalError(Some("bad status 200".into())))?;
    Ok(response)
}

fn json_out(status: u16, v: &Value) -> Result<Response, ErrorCode> {
    respond(status, "application/json", v.to_string())
}

fn fail(status: u16, why: &str) -> Result<Response, ErrorCode> {
    json_out(status, &json!({ "error": why }))
}

guestio::guest_p3_bearer!();
guestio::guest_p3_respond!();

/// Every route except `/`, `/health` and the auth routes must go through this.
fn who(req: &Request) -> Option<Principal> {
    authorizer::introspect(&bearer(req)?).ok()
}

fn ns(p: &Principal) -> String {
    format!("u/{}/", p.subject)
}

/// Backstop below the store's memory cap; generous because a photo arrives base64-encoded.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

// Never yields a truncated body; callers treat a failed or over-long read as empty.
guestio::guest_p3_read_body!(MAX_BODY_BYTES);

impl Guest for Component {
    async fn handle(req: Request) -> Result<Response, ErrorCode> {
        let full = req.get_path_with_query().unwrap_or_else(|| "/".into());
        let (path, query) = match full.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (full.clone(), String::new()),
        };
        let method = req.get_method();

        let bucket = match open() {
            Ok(b) => b,
            Err(e) => return fail(500, &format!("no store: {e}")),
        };

        match (&method, path.as_str()) {
            (Method::Get, "/health") => json_out(200, &json!({ "ok": true })),

            (Method::Get, "/api/prompt") => json_out(200, &json!({ "prompt": ident::prompt() })),

            (Method::Post, "/api/register") => {
                let body = read_body(req).await.unwrap_or_default();
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    return fail(400, "not json");
                };
                let email = v.get("email").and_then(Value::as_str).unwrap_or_default();
                let password = v.get("password").and_then(Value::as_str).unwrap_or_default();
                match accounts::register(email, password, "binder") {
                    Ok(p) => json_out(201, &json!({ "subject": p.subject })),
                    Err(e) => fail(409, &format!("{e:?}")),
                }
            }

            (Method::Post, "/api/login") => {
                let body = read_body(req).await.unwrap_or_default();
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    return fail(400, "not json");
                };
                let email = v.get("email").and_then(Value::as_str).unwrap_or_default();
                let password = v.get("password").and_then(Value::as_str).unwrap_or_default();
                match accounts::login(email, password, "binder") {
                    Ok(t) => json_out(200, &json!({ "access_token": t.access_token })),
                    // One message for every failure, so it cannot reveal which addresses have accounts.
                    Err(_) => fail(401, "invalid credentials"),
                }
            }

            (Method::Get, "/api/me") => match who(&req) {
                Some(p) => json_out(200, &json!({ "subject": p.subject, "roles": p.roles })),
                None => fail(401, "sign in"),
            },

            // The vision call takes up to a minute, so the upload only stores the picture and the work
            // runs on the event stream below.
            (Method::Post, "/api/photo") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let ns = ns(&who);
                let body = read_body(req).await.unwrap_or_default();
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    return fail(400, "not json");
                };
                let media_type =
                    v.get("media_type").and_then(Value::as_str).unwrap_or("image/jpeg").to_string();
                let Some(data) = v.get("data").and_then(Value::as_str) else {
                    return fail(400, "no image");
                };
                if decode_b64(data).is_none() {
                    return fail(400, "the image is not base64");
                }
                // Unique per upload without a random source.
                let id = format!("{}-{}", now(), data.len());
                let job = json!({ "media_type": media_type, "data": data });
                if let Err(e) = put_json(&bucket, &format!("{ns}job:{id}"), &job) {
                    return fail(500, &e);
                }
                json_out(202, &json!({ "job": id, "events": format!("/api/photo/{id}/events") }))
            }

            // Runs inside this request: an instance is per-request (ADR-0037), nothing could run it later.
            (Method::Get, p) if p.starts_with("/api/photo/") && p.ends_with("/events") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let ns = ns(&who);
                let id =
                    percent_decode(p.trim_start_matches("/api/photo/").trim_end_matches("/events"));
                let Some(job) = get_json::<Value>(&bucket, &format!("{ns}job:{id}")) else {
                    return fail(404, "no such job");
                };
                // Claimed by deleting it, so a reconnecting stream does not spend a second vision call.
                let _ = bucket.delete(&format!("{ns}job:{id}"));

                let media_type = job
                    .get("media_type")
                    .and_then(Value::as_str)
                    .unwrap_or("image/jpeg")
                    .to_string();
                let bytes = job
                    .get("data")
                    .and_then(Value::as_str)
                    .and_then(decode_b64)
                    .unwrap_or_default();

                stream_photo(bucket, ns, bytes, media_type)
            }

            (Method::Post, "/api/scan") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let ns = ns(&who);
                let body = read_body(req).await.unwrap_or_default();
                let answer = match serde_json::from_slice::<Value>(&body) {
                    Ok(v) => {
                        v.get("answer").and_then(Value::as_str).unwrap_or_default().to_string()
                    }
                    Err(_) => String::from_utf8_lossy(&body).to_string(),
                };
                match ident::parse(&answer) {
                    Ok(g) => {
                        let id = format!(
                            "{}-{}",
                            if g.set_code.is_empty() { "unknown" } else { &g.set_code },
                            if g.number.is_empty() {
                                g.name.replace(' ', "-").to_lowercase()
                            } else {
                                g.number.replace('/', "-")
                            }
                        );
                        let card = Card {
                            id: id.clone(),
                            name: g.name,
                            set_name: g.set_name,
                            set_code: g.set_code,
                            number: g.number,
                            rarity: g.rarity,
                            language: g.language,
                            printing: g.printing.map(printing_name).unwrap_or_default(),
                            condition: g.condition.map(condition_name).unwrap_or_default(),
                            graded: g
                                .graded
                                .map(|gr| format!("{} {}", gr.grader, gr.tenths as f64 / 10.0))
                                .unwrap_or_default(),
                            confidence: g.confidence,
                            needs_review: g
                                .needs_review
                                .into_iter()
                                .map(|f| if f == "variant" { "printing".to_string() } else { f })
                                .collect(),
                        };
                        if let Err(e) = put_json(&bucket, &format!("{ns}card:{id}"), &card) {
                            return fail(500, &e);
                        }
                        json_out(201, &serde_json::to_value(card).unwrap_or(Value::Null))
                    }
                    Err(e) => fail(422, &format!("{e:?}")),
                }
            }

            (Method::Post, "/api/cards") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let ns = ns(&who);
                let body = read_body(req).await.unwrap_or_default();
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    return fail(400, "not json");
                };
                let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                let name = s("name");
                if name.trim().is_empty() {
                    return fail(400, "a card needs a name");
                }
                let set_code = s("set_code").to_lowercase();
                let number = s("number");
                let id = format!(
                    "{}-{}",
                    if set_code.is_empty() { "unknown" } else { &set_code },
                    if number.is_empty() {
                        name.replace(' ', "-").to_lowercase()
                    } else {
                        number.replace('/', "-")
                    }
                );
                let card = Card {
                    id: id.clone(),
                    name,
                    set_name: s("set_name"),
                    set_code,
                    number,
                    rarity: s("rarity"),
                    language: s("language"),
                    printing: s("printing"),
                    condition: s("condition"),
                    graded: s("graded"),
                    confidence: 100,
                    needs_review: vec![],
                };
                if let Err(e) = put_json(&bucket, &format!("{ns}card:{id}"), &card) {
                    return fail(500, &e);
                }
                if let Some(paid) = v.get("paid_minor").and_then(Value::as_i64) {
                    let at = v.get("at").and_then(Value::as_u64).unwrap_or_else(now);
                    let ev = StoredEvent {
                        card_id: id.clone(),
                        kind: "acquired".into(),
                        quantity: v.get("quantity").and_then(Value::as_u64).unwrap_or(1) as u32,
                        unit_minor: paid,
                        currency: v
                            .get("currency")
                            .and_then(Value::as_str)
                            .unwrap_or("EUR")
                            .to_string(),
                        at,
                    };
                    let _ = put_json(&bucket, &event_key(&ns, &ev), &ev);
                }
                json_out(201, &serde_json::to_value(card).unwrap_or(Value::Null))
            }

            (Method::Post, "/api/swaps") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let ns = ns(&who);
                let body = read_body(req).await.unwrap_or_default();
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    return fail(400, "not json");
                };
                let str_of =
                    |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                let give = str_of("give");
                let want = str_of("want");
                if give.is_empty() || want.is_empty() {
                    return fail(400, "a swap needs a `give` and a `want`");
                }
                if get_json::<Card>(&bucket, &format!("{ns}card:{give}")).is_none() {
                    return fail(422, &format!("you do not have `{give}`"));
                }
                let value_minor = v.get("value_minor").and_then(Value::as_i64).unwrap_or(0);
                if value_minor <= 0 {
                    return fail(422, "a swap needs an agreed `value_minor` above zero");
                }
                let swap = Swap {
                    id: ids::short_code(10),
                    from: who.subject.clone(),
                    give,
                    want,
                    value_minor,
                    currency: v
                        .get("currency")
                        .and_then(Value::as_str)
                        .unwrap_or("EUR")
                        .to_uppercase(),
                    taken_by: String::new(),
                    at: v.get("at").and_then(Value::as_u64).unwrap_or_else(now),
                };
                // Outside any account's namespace: the other party must be able to read it.
                if let Err(e) = put_json(&bucket, &format!("swap:{}", swap.id), &swap) {
                    return fail(500, &e);
                }
                let square = qr::svg(&swap.id, qr::Ecc::Medium, 2).unwrap_or_default();
                json_out(
                    201,
                    &json!({
                        "id": swap.id,
                        "give": swap.give,
                        "want": swap.want,
                        "value_minor": swap.value_minor,
                        "currency": swap.currency,
                        "qr_svg": square,
                    }),
                )
            }

            // Anyone with the id may read an open swap (that is what the QR is for); taking it needs an account.
            (Method::Get, p) if p.starts_with("/api/swaps/") && !p.ends_with("/accept") => {
                let id = percent_decode(p.trim_start_matches("/api/swaps/"));
                match get_json::<Swap>(&bucket, &format!("swap:{id}")) {
                    None => fail(404, "no such swap"),
                    Some(s) => json_out(
                        200,
                        &json!({
                            "id": s.id,
                            "give": s.give,
                            "want": s.want,
                            "value_minor": s.value_minor,
                            "currency": s.currency,
                            "open": s.taken_by.is_empty(),
                        }),
                    ),
                }
            }

            (Method::Post, p) if p.starts_with("/api/swaps/") && p.ends_with("/accept") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let taker_ns = ns(&who);
                let id =
                    percent_decode(p.trim_start_matches("/api/swaps/").trim_end_matches("/accept"));
                let Some(mut swap) = get_json::<Swap>(&bucket, &format!("swap:{id}")) else {
                    return fail(404, "no such swap");
                };
                if !swap.taken_by.is_empty() {
                    return fail(409, "that swap has already been taken");
                }
                if swap.from == who.subject {
                    return fail(422, "you cannot take your own swap");
                }
                let offerer_ns = format!("u/{}/", swap.from);

                // Both sides are checked before anything moves; a half swap leaves someone with two cards.
                let Some(given) =
                    get_json::<Card>(&bucket, &format!("{offerer_ns}card:{}", swap.give))
                else {
                    return fail(409, "the offered card is no longer in that collection");
                };
                let Some(wanted) =
                    get_json::<Card>(&bucket, &format!("{taker_ns}card:{}", swap.want))
                else {
                    return fail(422, &format!("you do not have `{}`", swap.want));
                };

                let at = now();
                let ev = |card_id: &str, kind: &str| StoredEvent {
                    card_id: card_id.to_string(),
                    kind: kind.to_string(),
                    quantity: 1,
                    unit_minor: swap.value_minor,
                    currency: swap.currency.clone(),
                    at,
                };

                for (namespace, card, arriving) in
                    [(&offerer_ns, &given, &wanted), (&taker_ns, &wanted, &given)]
                {
                    let _ = bucket.delete(&format!("{namespace}card:{}", card.id));
                    if let Err(e) =
                        put_json(&bucket, &format!("{namespace}card:{}", arriving.id), arriving)
                    {
                        return fail(500, &e);
                    }
                    let leaving = ev(&card.id, "disposed");
                    let joining = ev(&arriving.id, "acquired");
                    let _ = put_json(&bucket, &event_key(namespace, &leaving), &leaving);
                    let _ = put_json(&bucket, &event_key(namespace, &joining), &joining);
                }

                swap.taken_by = who.subject.clone();
                if let Err(e) = put_json(&bucket, &format!("swap:{}", swap.id), &swap) {
                    return fail(500, &e);
                }
                json_out(
                    200,
                    &json!({
                        "id": swap.id,
                        "you_received": given.id,
                        "you_gave": wanted.id,
                        "value_minor": swap.value_minor,
                        "currency": swap.currency,
                        "events_written": 4,
                    }),
                )
            }

            // `?name=` picks the reader: sniffing bytes turns a semicolon-delimited export into one column.
            // All or nothing: a half-applied import cannot be told apart and a retry duplicates it.
            (Method::Post, "/api/cards/bulk") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let ns = ns(&who);
                let name = param(&query, "name").unwrap_or_else(|| "upload.csv".into());
                let body = read_body(req).await.unwrap_or_default();
                if body.is_empty() {
                    return fail(400, "no file in the body");
                }

                let sheet = match sheet::read(&name, &body) {
                    Ok(s) => s,
                    Err(e) => return fail(400, &format!("{e:?}")),
                };

                let key = |h: &str| -> String {
                    h.chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ' ')
                        .collect::<String>()
                        .to_lowercase()
                        .replace(' ', "_")
                        .trim_matches('_')
                        .to_string()
                };
                let cols: Vec<String> = sheet.header.iter().map(|h| key(h)).collect();
                let at_col = |row: &sheet::Row, want: &str| -> String {
                    cols.iter()
                        .position(|c| c == want)
                        .and_then(|i| row.cells.get(i))
                        .map(|v| v.trim().to_string())
                        .unwrap_or_default()
                };
                if !cols.iter().any(|c| c == "name") {
                    return fail(422, &format!("no `name` column — found {:?}", sheet.header));
                }

                // `row` is 1-based and counts the header: the row number the spreadsheet shows.
                let mut staged: Vec<(Card, Option<StoredEvent>)> = Vec::new();
                let mut problems: Vec<Value> = Vec::new();
                for (i, row) in sheet.rows.iter().enumerate() {
                    let line = i as u32 + 2;
                    let mut bad = |why: &str| {
                        problems.push(json!({ "row": line, "problem": why }));
                    };
                    let card_name = at_col(row, "name");
                    if card_name.is_empty() {
                        bad("no name");
                        continue;
                    }
                    let quantity = at_col(row, "quantity");
                    let quantity = if quantity.is_empty() {
                        1u32
                    } else {
                        match quantity.parse::<u32>() {
                            Ok(0) => {
                                bad("a quantity of zero");
                                continue;
                            }
                            Ok(n) => n,
                            Err(_) => {
                                bad(&format!("quantity `{quantity}` is not a whole number"));
                                continue;
                            }
                        }
                    };
                    let paid = at_col(row, "paid_minor");
                    let paid = if paid.is_empty() {
                        None
                    } else {
                        // Minor units only: a decimal point is euros in a cents column, and rounding it is wrong.
                        match paid.parse::<i64>() {
                            Ok(v) => Some(v),
                            Err(_) => {
                                bad(&format!(
                                    "paid_minor `{paid}` is not a whole number of minor units"
                                ));
                                continue;
                            }
                        }
                    };

                    let set_code = at_col(row, "set_code").to_lowercase();
                    let number = at_col(row, "number");
                    let id = format!(
                        "{}-{}",
                        if set_code.is_empty() { "unknown" } else { &set_code },
                        if number.is_empty() {
                            card_name.replace(' ', "-").to_lowercase()
                        } else {
                            number.replace('/', "-")
                        }
                    );
                    let card = Card {
                        id: id.clone(),
                        name: card_name,
                        set_name: at_col(row, "set_name"),
                        set_code,
                        number,
                        rarity: at_col(row, "rarity"),
                        language: at_col(row, "language"),
                        printing: at_col(row, "printing"),
                        condition: at_col(row, "condition"),
                        graded: at_col(row, "graded"),
                        confidence: 100,
                        needs_review: vec![],
                    };
                    let event = paid.map(|unit_minor| StoredEvent {
                        card_id: id,
                        kind: "acquired".into(),
                        quantity,
                        unit_minor,
                        currency: {
                            let c = at_col(row, "currency");
                            if c.is_empty() {
                                "EUR".to_string()
                            } else {
                                c.to_uppercase()
                            }
                        },
                        at: {
                            let a = at_col(row, "at");
                            a.parse::<u64>().unwrap_or_else(|_| now())
                        },
                    });
                    staged.push((card, event));
                }

                if !problems.is_empty() {
                    return json_out(
                        422,
                        &json!({
                            "error": "nothing was imported",
                            "why": "every row is checked before any is written, so a \
                                    second attempt cannot duplicate a partial one",
                            "rows": sheet.rows.len(),
                            "problems": problems,
                        }),
                    );
                }

                let mut added = 0u32;
                let mut updated = 0u32;
                let mut priced = 0u32;
                for (card, event) in staged {
                    let k = format!("{ns}card:{}", card.id);
                    let existed = bucket.get(&k).ok().flatten().is_some();
                    if let Err(e) = put_json(&bucket, &k, &card) {
                        return fail(500, &e);
                    }
                    if existed {
                        updated += 1;
                    } else {
                        added += 1;
                    }
                    if let Some(ev) = event {
                        let _ = put_json(&bucket, &event_key(&ns, &ev), &ev);
                        priced += 1;
                    }
                }
                json_out(
                    201,
                    &json!({
                        "sheet": sheet.sheet_name,
                        "rows": added + updated,
                        "added": added,
                        "updated": updated,
                        "with_a_purchase": priced,
                    }),
                )
            }

            // Its events stay: deleting the row must not rewrite a realised gain.
            (Method::Delete, "/api/cards") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let body = read_body(req).await.unwrap_or_default();
                let id = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_default();
                if id.is_empty() {
                    return fail(400, "which card");
                }
                match bucket.delete(&format!("{}card:{id}", ns(&who))) {
                    Ok(()) => json_out(200, &json!({ "deleted": id })),
                    Err(e) => fail(500, &format!("{e:?}")),
                }
            }

            (Method::Get, p) if p.starts_with("/api/cards/") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let ns = ns(&who);
                let id = percent_decode(p.trim_start_matches("/api/cards/"));
                let Some(card) = get_json::<Card>(&bucket, &format!("{ns}card:{id}")) else {
                    return fail(404, "no such card");
                };

                let events: Vec<StoredEvent> = scan::<StoredEvent>(&bucket, &format!("{ns}event:"))
                    .into_iter()
                    .filter(|e| e.card_id == id)
                    .collect();
                let held: i64 = events
                    .iter()
                    .map(|e| {
                        if e.kind == "disposed" {
                            -(e.quantity as i64)
                        } else {
                            e.quantity as i64
                        }
                    })
                    .sum();

                let stored: Vec<StoredQuote> =
                    scan::<StoredQuote>(&bucket, &format!("quote:{id}:"));
                let quotes: Vec<ph::Quote> = stored
                    .iter()
                    .map(|q| ph::Quote {
                        unit_minor: q.unit_minor,
                        currency: q.currency.clone(),
                        kind: ph::QuoteKind::Market,
                        source: "binder".into(),
                        at: q.at,
                    })
                    .collect();

                let until = now();

                // Valued by `portfolio:value` (FIFO), so the card agrees with the portfolio total.
                let one_card: Vec<pv::Event> = events
                    .iter()
                    .map(|e| pv::Event {
                        item_id: e.card_id.clone(),
                        kind: if e.kind == "disposed" {
                            pv::EventKind::Disposed
                        } else {
                            pv::EventKind::Acquired
                        },
                        quantity: e.quantity,
                        unit_minor: e.unit_minor,
                        currency: e.currency.clone(),
                        at: e.at,
                    })
                    .collect();
                let valued = pv::value_at(&one_card, &quotes_for(&bucket), until).ok();
                let basis = valued.as_ref().map(|v| v.cost_basis_minor).unwrap_or(0);
                let realised = valued.as_ref().map(|v| v.realised_minor).unwrap_or(0);

                let days = param(&query, "days").and_then(|d| d.parse::<u64>().ok()).unwrap_or(90);
                let earliest = stored.iter().map(|q| q.at).min().unwrap_or(until);
                let window = if days == 0 { until.saturating_sub(earliest) } else { days * 86_400 };
                let step = if window > 400 * 86_400 { 7 * 86_400 } else { 86_400 };
                let series = ph::series(
                    &quotes,
                    ph::QuoteKind::Market,
                    until.saturating_sub(window.max(step)),
                    until,
                    step,
                )
                .unwrap_or_default();
                let price = ph::at(&quotes, ph::QuoteKind::Market, until).ok();

                let mut changes: Vec<Change> = scan::<Change>(&bucket, &format!("{ns}change:"))
                    .into_iter()
                    .filter(|c| c.card_id == id)
                    .collect();
                changes.sort_by_key(|a| std::cmp::Reverse(a.at));

                let mut evs = events;
                evs.sort_by_key(|a| std::cmp::Reverse(a.at));

                json_out(
                    200,
                    &json!({
                        "card": card,
                        "held": held.max(0),
                        "cost_basis_minor": basis,
                        "realised_minor": realised,
                        "price_minor": price.as_ref().map(|o| o.unit_minor),
                        "currency": price.as_ref().map(|o| o.currency.clone()).unwrap_or_else(|| "EUR".into()),
                        "price_age_days": price.as_ref().map(|o| o.age_seconds / 86_400),
                        "value_minor": price.as_ref().map(|o| held.max(0) * o.unit_minor),
                        "series": series.iter().map(|s| json!({
                            "at": s.at, "unit_minor": s.unit_minor, "carried": s.carried
                        })).collect::<Vec<_>>(),
                        "quotes": stored.iter().map(|q| json!({
                            "at": q.at, "unit_minor": q.unit_minor, "currency": q.currency
                        })).collect::<Vec<_>>(),
                        "events": evs.iter().map(|e| json!({
                            "at": e.at, "kind": e.kind, "quantity": e.quantity,
                            "unit_minor": e.unit_minor, "currency": e.currency
                        })).collect::<Vec<_>>(),
                        "changes": changes,
                    }),
                )
            }

            (Method::Get, "/api/cards") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let ns = ns(&who);
                let cards: Vec<Card> = scan(&bucket, &format!("{ns}card:"));
                let decks: Vec<Deck> = scan(&bucket, &format!("{ns}deck:"));

                let mut held: std::collections::BTreeMap<String, (i64, i64)> = Default::default();
                for e in scan::<StoredEvent>(&bucket, &format!("{ns}event:")) {
                    let n = e.quantity as i64;
                    let slot = held.entry(e.card_id).or_insert((0, 0));
                    if e.kind == "disposed" {
                        slot.0 -= n;
                    } else {
                        slot.0 += n;
                        slot.1 += n * e.unit_minor;
                    }
                }

                let all_quotes = scan::<StoredQuote>(&bucket, "quote:");
                let at = now();
                json_out(
                    200,
                    &json!({
                        "cards": cards.iter().map(|c| {
                            let mut v = serde_json::to_value(c).unwrap_or(Value::Null);
                            let used: Vec<&str> = decks.iter()
                                .filter(|d| d.slots.iter().any(|s| s.card_id == c.id))
                                .map(|d| d.name.as_str())
                                .collect();
                            v["in_decks"] = json!(used);

                            let (qty, basis) = held.get(&c.id).copied().unwrap_or((0, 0));
                            v["held"] = json!(qty.max(0));
                            v["cost_basis_minor"] = json!(basis);

                            // `price:history`'s carry-forward, not the newest row, so last week's price still counts.
                            let quotes: Vec<ph::Quote> = all_quotes
                                .iter()
                                .filter(|q| q.card_id == c.id)
                                .map(|q| ph::Quote {
                                    unit_minor: q.unit_minor,
                                    currency: q.currency.clone(),
                                    kind: ph::QuoteKind::Market,
                                    source: "binder".into(),
                                    at: q.at,
                                })
                                .collect();
                            match ph::at(&quotes, ph::QuoteKind::Market, at) {
                                Ok(o) => {
                                    v["price_minor"] = json!(o.unit_minor);
                                    v["currency"] = json!(o.currency);
                                    v["priced_at"] = json!(o.observed_at);
                                    v["price_age_days"] = json!(o.age_seconds / 86_400);
                                    v["value_minor"] = json!(qty.max(0) * o.unit_minor);
                                }
                                // Absent, not zero: unpriced is not worthless.
                                Err(_) => {
                                    v["price_minor"] = Value::Null;
                                    v["value_minor"] = Value::Null;
                                }
                            }
                            v
                        }).collect::<Vec<_>>()
                    }),
                )
            }

            (Method::Patch, "/api/cards") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let ns = ns(&who);
                let body = read_body(req).await.unwrap_or_default();
                let Ok(patch) = serde_json::from_slice::<Value>(&body) else {
                    return fail(400, "not json");
                };
                let Some(id) = patch.get("id").and_then(Value::as_str) else {
                    return fail(400, "which card");
                };
                let Some(mut card) = get_json::<Card>(&bucket, &format!("{ns}card:{id}")) else {
                    return fail(404, "no such card");
                };
                let at = now();
                let mut changes: Vec<Change> = Vec::new();
                for (field, slot) in [
                    ("name", &mut card.name),
                    ("set_name", &mut card.set_name),
                    ("set_code", &mut card.set_code),
                    ("number", &mut card.number),
                    ("rarity", &mut card.rarity),
                    ("language", &mut card.language),
                    ("printing", &mut card.printing),
                    ("condition", &mut card.condition),
                    ("graded", &mut card.graded),
                ] {
                    if let Some(v) = patch.get(field).and_then(Value::as_str) {
                        if slot.as_str() != v {
                            changes.push(Change {
                                card_id: id.to_string(),
                                field: field.to_string(),
                                from: slot.clone(),
                                to: v.to_string(),
                                at,
                            });
                        }
                        *slot = v.to_string();
                        card.needs_review.retain(|r| r != field);
                    }
                }
                for c in &changes {
                    // Keyed by field, not by index within the save: an index restarts at zero and overwrote history.
                    let _ = put_json(&bucket, &format!("{ns}change:{at:020}:{id}:{}", c.field), c);
                }
                if let Err(e) = put_json(&bucket, &format!("{ns}card:{id}"), &card) {
                    return fail(500, &e);
                }
                json_out(200, &serde_json::to_value(card).unwrap_or(Value::Null))
            }

            (Method::Post, "/api/events") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let ns = ns(&who);
                let body = read_body(req).await.unwrap_or_default();
                let Ok(e) = serde_json::from_slice::<Value>(&body) else {
                    return fail(400, "not json");
                };
                let at = e.get("at").and_then(Value::as_u64).unwrap_or_else(now);
                let ev = StoredEvent {
                    card_id: e
                        .get("card_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    kind: e.get("kind").and_then(Value::as_str).unwrap_or("acquired").to_string(),
                    quantity: e.get("quantity").and_then(Value::as_u64).unwrap_or(1) as u32,
                    unit_minor: e.get("unit_minor").and_then(Value::as_i64).unwrap_or(0),
                    currency: e
                        .get("currency")
                        .and_then(Value::as_str)
                        .unwrap_or("EUR")
                        .to_string(),
                    at,
                };
                if ev.card_id.is_empty() {
                    return fail(400, "which card");
                }
                // Refused here: `portfolio:value` refuses an oversold log, which would break `/api/portfolio`
                // for every screen.
                if ev.kind == "disposed" {
                    let held: i64 = scan::<StoredEvent>(&bucket, &format!("{ns}event:"))
                        .into_iter()
                        .filter(|e| e.card_id == ev.card_id && e.at <= at)
                        .map(|e| {
                            if e.kind == "disposed" {
                                -(e.quantity as i64)
                            } else {
                                e.quantity as i64
                            }
                        })
                        .sum();
                    if (ev.quantity as i64) > held {
                        return fail(
                            409,
                            &format!(
                                "you hold {held} of {} at that date, so {} cannot be sold",
                                ev.card_id, ev.quantity
                            ),
                        );
                    }
                }
                let key = event_key(&ns, &ev);
                if let Err(e) = put_json(&bucket, &key, &ev) {
                    return fail(500, &e);
                }
                json_out(201, &serde_json::to_value(ev).unwrap_or(Value::Null))
            }

            (Method::Delete, "/api/events") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let body = read_body(req).await.unwrap_or_default();
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    return fail(400, "not json");
                };
                let (Some(card), Some(at)) =
                    (v.get("card_id").and_then(Value::as_str), v.get("at").and_then(Value::as_u64))
                else {
                    return fail(400, "which event — card_id and at");
                };
                // The exact key when fully named; an (instant, card) prefix would remove a buy and a sell
                // sharing a second.
                let ns = ns(&who);
                let found: Vec<String> = match (
                    v.get("kind").and_then(Value::as_str),
                    v.get("quantity").and_then(Value::as_u64),
                    v.get("unit_minor").and_then(Value::as_i64),
                ) {
                    (Some(kind), Some(quantity), Some(unit_minor)) => {
                        let key = event_key(
                            &ns,
                            &StoredEvent {
                                card_id: card.to_string(),
                                kind: kind.to_string(),
                                quantity: quantity as u32,
                                unit_minor,
                                currency: String::new(),
                                at,
                            },
                        );
                        if bucket.exists(&key).unwrap_or(false) {
                            vec![key]
                        } else {
                            vec![]
                        }
                    }
                    _ => keys_under(&bucket, &format!("{ns}event:{:020}:{card}:", at)),
                };
                for k in &found {
                    let _ = bucket.delete(k);
                }
                json_out(200, &json!({ "deleted": found.len(), "card_id": card, "at": at }))
            }

            (Method::Post, "/api/quotes") => {
                let body = read_body(req).await.unwrap_or_default();
                let Ok(q) = serde_json::from_slice::<Value>(&body) else {
                    return fail(400, "not json");
                };
                let at = q.get("at").and_then(Value::as_u64).unwrap_or_else(now);
                let quote = StoredQuote {
                    card_id: q
                        .get("card_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    unit_minor: q.get("unit_minor").and_then(Value::as_i64).unwrap_or(0),
                    currency: q
                        .get("currency")
                        .and_then(Value::as_str)
                        .unwrap_or("EUR")
                        .to_string(),
                    at,
                };
                if quote.card_id.is_empty() {
                    return fail(400, "which card");
                }
                let key = format!("quote:{}:{:020}", quote.card_id, at);
                if let Err(e) = put_json(&bucket, &key, &quote) {
                    return fail(500, &e);
                }
                json_out(201, &serde_json::to_value(quote).unwrap_or(Value::Null))
            }

            (Method::Get, p) if p.starts_with("/api/price/") => {
                let card = percent_decode(p.trim_start_matches("/api/price/"));
                let quotes: Vec<ph::Quote> =
                    scan::<StoredQuote>(&bucket, &format!("quote:{card}:"))
                        .into_iter()
                        .map(|q| ph::Quote {
                            unit_minor: q.unit_minor,
                            currency: q.currency,
                            kind: ph::QuoteKind::Market,
                            source: "binder".into(),
                            at: q.at,
                        })
                        .collect();
                let until = now();
                let since = until.saturating_sub(90 * 86_400);
                match ph::series(&quotes, ph::QuoteKind::Market, since, until, 86_400) {
                    Ok(points) => json_out(
                        200,
                        &json!({
                            "card_id": card,
                            "points": points.iter().map(|p| json!({
                                "at": p.at, "unit_minor": p.unit_minor, "carried": p.carried
                            })).collect::<Vec<_>>()
                        }),
                    ),
                    Err(e) => fail(422, &format!("{e:?}")),
                }
            }

            (Method::Get, "/api/portfolio") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let events = events_for(&bucket, &ns(&who));
                let quotes = quotes_for(&bucket);
                let until = now();
                let days = param(&query, "days").and_then(|d| d.parse::<u64>().ok());
                let earliest = events.iter().map(|e| e.at).min().unwrap_or(until);
                let window = match days {
                    Some(0) => until.saturating_sub(earliest),
                    Some(d) => d * 86_400,
                    None => 90 * 86_400,
                };
                let step = param(&query, "step")
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|s| *s > 0)
                    .unwrap_or(if window > 400 * 86_400 { 7 * 86_400 } else { 86_400 });
                match pv::value_at(&events, &quotes, until) {
                    Ok(v) => {
                        let since = until.saturating_sub(window.max(step));
                        let points =
                            pv::series(&events, &quotes, since, until, step).unwrap_or_default();
                        json_out(
                            200,
                            &json!({
                                "cost_basis_minor": v.cost_basis_minor,
                                "market_value_minor": v.market_value_minor,
                                "unrealised_minor": v.unrealised_minor,
                                "realised_minor": v.realised_minor,
                                "currency": v.currency,
                                "unquoted": v.unquoted,
                                "since": until.saturating_sub(window.max(step)),
                                "until": until,
                                "step": step,
                                "earliest_event": earliest,
                                "series": points.iter().map(|p| json!({
                                    "at": p.at,
                                    "market_value_minor": p.market_value_minor,
                                    "cost_basis_minor": p.cost_basis_minor,
                                    "realised_minor": p.realised_minor,
                                    "unrealised_minor": p.market_value_minor - p.cost_basis_minor,
                                    "unquoted": p.unquoted,
                                })).collect::<Vec<_>>()
                            }),
                        )
                    }
                    Err(pv::ValueError::Empty) => json_out(
                        200,
                        &json!({
                            "cost_basis_minor": 0, "market_value_minor": 0,
                            "unrealised_minor": 0, "realised_minor": 0,
                            "currency": "EUR", "unquoted": 0, "series": [], "empty": true
                        }),
                    ),
                    // 200 with zeroes, not a 422: one bad event used to take out every screen, leaving none to fix it.
                    Err(e) => {
                        let (problem, card) = match &e {
                            pv::ValueError::OversoldAt((card, at, held, disposed)) => (
                                format!(
                                    "{card} was sold {disposed} on {at} with {held} held. \
                                     Delete that sale, or record what you bought first."
                                ),
                                card.clone(),
                            ),
                            pv::ValueError::MixedCurrency((want, got)) => (
                                format!("this collection has both {want} and {got} in it"),
                                String::new(),
                            ),
                            pv::ValueError::ZeroQuantity((card, _)) => {
                                (format!("{card} has an event for zero copies"), card.clone())
                            }
                            other => (format!("{other:?}"), String::new()),
                        };
                        json_out(
                            200,
                            &json!({
                                "cost_basis_minor": 0, "market_value_minor": 0,
                                "unrealised_minor": 0, "realised_minor": 0,
                                "currency": "EUR", "unquoted": 0, "series": [],
                                "blocked": problem,
                                "blocked_card": card,
                            }),
                        )
                    }
                }
            }

            (Method::Get, "/api/decks") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let decks: Vec<Deck> = scan(&bucket, &format!("{}deck:", ns(&who)));
                json_out(200, &json!({ "decks": decks }))
            }

            (Method::Post, "/api/decks") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let body = read_body(req).await.unwrap_or_default();
                let name = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_default();
                if name.trim().is_empty() {
                    return fail(400, "a deck needs a name");
                }
                let key = format!("{}deck:{name}", ns(&who));
                if get_json::<Deck>(&bucket, &key).is_some() {
                    return fail(409, "you already have a deck by that name");
                }
                let d = Deck { name, slots: vec![] };
                if let Err(e) = put_json(&bucket, &key, &d) {
                    return fail(500, &e);
                }
                json_out(201, &serde_json::to_value(d).unwrap_or(Value::Null))
            }

            // A deck refers to the collection; it does not consume cards.
            (Method::Post, p) if p.starts_with("/api/decks/") && p.ends_with("/slots") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let ns = ns(&who);
                let name =
                    percent_decode(p.trim_start_matches("/api/decks/").trim_end_matches("/slots"));
                let key = format!("{ns}deck:{name}");
                let Some(mut d) = get_json::<Deck>(&bucket, &key) else {
                    return fail(404, "no such deck");
                };
                let body = read_body(req).await.unwrap_or_default();
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    return fail(400, "not json");
                };
                let card_id =
                    v.get("card_id").and_then(Value::as_str).unwrap_or_default().to_string();
                if card_id.is_empty() {
                    return fail(400, "which card");
                }
                let held = get_json::<Card>(&bucket, &format!("{ns}card:{card_id}"));
                let card_name = v
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| held.as_ref().map(|c| c.name.clone()))
                    .unwrap_or_else(|| card_id.clone());
                let quantity = v.get("quantity").and_then(Value::as_u64).unwrap_or(1) as u32;
                let kind =
                    v.get("kind").and_then(Value::as_str).unwrap_or("basic-pokemon").to_string();

                d.slots.retain(|s| s.card_id != card_id);
                if quantity > 0 {
                    d.slots.push(DeckSlot { card_id, name: card_name, kind, quantity });
                }
                d.slots.sort_by(|a, b| a.card_id.cmp(&b.card_id));
                if let Err(e) = put_json(&bucket, &key, &d) {
                    return fail(500, &e);
                }
                json_out(200, &serde_json::to_value(d).unwrap_or(Value::Null))
            }

            (Method::Delete, "/api/decks") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let body = read_body(req).await.unwrap_or_default();
                let name = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_default();
                if name.is_empty() {
                    return fail(400, "which deck");
                }
                match bucket.delete(&format!("{}deck:{name}", ns(&who))) {
                    Ok(()) => json_out(200, &json!({ "deleted": name })),
                    Err(e) => fail(500, &format!("{e:?}")),
                }
            }

            (Method::Put, "/api/decks") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let body = read_body(req).await.unwrap_or_default();
                let Ok(d) = serde_json::from_slice::<Deck>(&body) else {
                    return fail(400, "not a deck");
                };
                if d.name.is_empty() {
                    return fail(400, "a deck needs a name");
                }
                if let Err(e) = put_json(&bucket, &format!("{}deck:{}", ns(&who), d.name), &d) {
                    return fail(500, &e);
                }
                json_out(200, &serde_json::to_value(d).unwrap_or(Value::Null))
            }

            (Method::Get, p) if p.starts_with("/api/decks/") => {
                let Some(who) = who(&req) else { return fail(401, "sign in") };
                let ns = ns(&who);
                let name = percent_decode(p.trim_start_matches("/api/decks/"));
                let Some(d) = get_json::<Deck>(&bucket, &format!("{ns}deck:{name}")) else {
                    return fail(404, "no such deck");
                };

                let slots: Vec<deck::Slot> = d
                    .slots
                    .iter()
                    .map(|s| deck::Slot {
                        card_id: s.card_id.clone(),
                        name: s.name.clone(),
                        kind: kind_of(&s.kind),
                        quantity: s.quantity,
                    })
                    .collect();

                let mut held: std::collections::BTreeMap<String, i64> = Default::default();
                for e in scan::<StoredEvent>(&bucket, &format!("{ns}event:")) {
                    let n = e.quantity as i64;
                    *held.entry(e.card_id).or_insert(0) +=
                        if e.kind == "disposed" { -n } else { n };
                }
                let owned: Vec<deck::Owned> = held
                    .into_iter()
                    .filter(|(_, n)| *n > 0)
                    .map(|(card_id, n)| deck::Owned { card_id, quantity: n as u32 })
                    .collect();

                let mut newest: std::collections::BTreeMap<String, StoredQuote> =
                    Default::default();
                for q in scan::<StoredQuote>(&bucket, "quote:") {
                    newest
                        .entry(q.card_id.clone())
                        .and_modify(|c| {
                            if q.at > c.at {
                                *c = q.clone();
                            }
                        })
                        .or_insert(q);
                }
                let prices: Vec<deck::Price> = newest
                    .into_values()
                    .map(|q| deck::Price {
                        card_id: q.card_id,
                        unit_minor: q.unit_minor,
                        currency: q.currency,
                    })
                    .collect();

                let why = deck::legality(&slots);
                let short = deck::shortfall(&slots, &owned, &prices, "EUR");
                json_out(
                    200,
                    &json!({
                        "name": d.name,
                        "cards": d.slots.iter().map(|s| s.quantity).sum::<u32>(),
                        "legal": why.is_empty(),
                        "illegal": why.iter().map(|i| match i {
                            deck::Illegal::WrongSize(n) =>
                                json!({ "rule": "size", "detail": format!("{n} cards, not 60") }),
                            deck::Illegal::TooManyOfAName((name, n)) =>
                                json!({ "rule": "four-copies", "detail": format!("{n} × {name}, across every printing") }),
                            deck::Illegal::NoBasicPokemon =>
                                json!({ "rule": "basic", "detail": "no Basic Pokémon to start with" }),
                            deck::Illegal::ZeroQuantity(id) =>
                                json!({ "rule": "empty-slot", "detail": id }),
                        }).collect::<Vec<_>>(),
                        "slots": d.slots.iter().map(|s| json!({
                            "card_id": s.card_id, "name": s.name,
                            "kind": kind_name(kind_of(&s.kind)), "quantity": s.quantity
                        })).collect::<Vec<_>>(),
                        "missing": short.missing.iter().map(|m| json!({
                            "card_id": m.card_id, "name": m.name,
                            "quantity": m.quantity, "cost_minor": m.cost_minor
                        })).collect::<Vec<_>>(),
                        "cost_minor": short.cost_minor,
                        "currency": short.currency,
                        "unpriced": short.unpriced,
                    }),
                )
            }

            _ => fail(404, "no such route"),
        }
    }
}

bindings::export!(Component with_types_in bindings);
