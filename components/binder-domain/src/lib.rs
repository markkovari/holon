//! `binder-domain` — keep a Pokémon card collection: scan a photo into fields, record what you paid, and see what it is worth
//!
//! The app is the composition. Every number here comes from a capability this
//! component imports, and none of the arithmetic is in this file:
//!
//! * `card:identify` reads a vision model's answer into typed fields and says which
//!   of them a person should check;
//! * `price:history` decides what a card was worth at an instant, carrying the last
//!   quote across the days the market did not trade;
//! * `portfolio:value` turns the buy/sell log into cost basis, realised and
//!   unrealised gain, and the series a chart is drawn from.
//!
//! What is genuinely this component's is the HTTP surface and the storage. That is
//! the split ADR-0095 requires — the pieces meet through WIT, so each is testable on
//! its own, and each of those three is tested by a held-out specification that was
//! written before its implementation existed.
//!
//! ## Storage
//!
//! Three key spaces in one bucket, which the linker names after the app (ADR-0023):
//! `card:<id>` a card, `event:<seq>` an acquisition or disposal, `quote:<card>:<at>`
//! an observed price. Lists are rebuilt by prefix scan rather than kept as an index,
//! because a collection is small and an index is a second thing to keep right.

#[allow(warnings)]
mod bindings;

use bindings::card::identify::identifier as ident;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::portfolio::value::valuation as pv;
use bindings::price::history::history as ph;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use bindings::wasi::keyvalue::store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

struct Component;

// `"default"` is what every other domain in this repository opens, and the host
// names the bucket after the app regardless (ADR-0023) — the guest string does not
// choose the isolation boundary, which is the whole point of ADR-0015. An empty name
// is simply not a bucket the host has.
const BUCKET: &str = "default";
const PAGE: &str = include_str!("page.html");

/// A card as the collection holds it: what the model guessed, plus whatever a person
/// has since corrected. `needs_review` survives the round trip so a screen can keep
/// showing what is still a guess.
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

/// One acquisition or disposal. Mirrors `portfolio:value`'s event, because the app
/// stores what the capability will be asked to value rather than a shape of its own.
#[derive(Serialize, Deserialize, Clone)]
struct StoredEvent {
    card_id: String,
    /// `acquired` or `disposed`.
    kind: String,
    quantity: u32,
    unit_minor: i64,
    currency: String,
    at: u64,
}

/// One observed price for a card.
#[derive(Serialize, Deserialize, Clone)]
struct StoredQuote {
    card_id: String,
    unit_minor: i64,
    currency: String,
    at: u64,
}

/// The contract's own word for a printing, not Rust's `Debug`. `VariantKind::Holo`
/// on a screen is an implementation detail leaking into a collection.
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

/// The words the singles market uses, for the same reason.
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

fn now() -> u64 {
    wall_clock::now().seconds
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

/// Everything under a prefix. A collection is small enough that scanning beats
/// maintaining an index, and an index that drifts is a lie with a fast path.
fn scan<T: for<'de> Deserialize<'de>>(b: &store::Bucket, prefix: &str) -> Vec<T> {
    // Keys are collected into a SET before anything is read, and the walk stops when
    // a page adds nothing new.
    //
    // Not defensive programming — a measured bug. `wasi:keyvalue`'s `list-keys` is a
    // draft and a backend is free to hand back a cursor that does not advance; this
    // host returns the whole key space with a cursor attached, so the obvious
    // "loop until the cursor is none" read every key three times. The collection then
    // reported forty commons as a hundred and twenty, and a €20.00 realised gain as
    // €60.00 — every number tripled, and every one of them still plausible.
    let mut keys = std::collections::BTreeSet::new();
    let mut cursor = None;
    loop {
        let Ok(page) = b.list_keys(cursor.clone()) else { break };
        let before = keys.len();
        keys.extend(page.keys.into_iter().filter(|k| k.starts_with(prefix)));
        match page.cursor {
            Some(c) if keys.len() > before => cursor = Some(c),
            _ => break,
        }
    }
    keys.iter().filter_map(|k| get_json(b, k)).collect()
}

fn events_for(b: &store::Bucket) -> Vec<pv::Event> {
    scan::<StoredEvent>(b, "event:")
        .into_iter()
        .map(|e| pv::Event {
            item_id: e.card_id,
            kind: if e.kind == "disposed" { pv::EventKind::Disposed } else { pv::EventKind::Acquired },
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

// ---- HTTP ---------------------------------------------------------------

fn respond(out: ResponseOutparam, status: u16, content_type: &str, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[content_type.as_bytes().to_vec()]);
    let resp = OutgoingResponse::new(headers);
    let _ = resp.set_status_code(status);
    let out_body = resp.body().expect("a response has a body");
    ResponseOutparam::set(out, Ok(resp));
    {
        let stream = out_body.write().expect("a body has a stream");
        // Chunked to stay under the stream's own limit; the page is larger than one
        // permitted write and a silent truncation would serve half a document.
        for chunk in body.chunks(4096) {
            if stream.blocking_write_and_flush(chunk).is_err() {
                return;
            }
        }
    }
    let _ = OutgoingBody::finish(out_body, None);
}

fn json_out(out: ResponseOutparam, status: u16, v: &Value) {
    respond(out, status, "application/json", v.to_string().as_bytes());
}

fn fail(out: ResponseOutparam, status: u16, why: &str) {
    json_out(out, status, &json!({ "error": why }));
}

fn read_body(req: &IncomingRequest) -> Vec<u8> {
    let Ok(body) = req.consume() else { return Vec::new() };
    let Ok(stream) = body.stream() else { return Vec::new() };
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }
    buf
}

impl Guest for Component {
    fn handle(req: IncomingRequest, out: ResponseOutparam) {
        let path = req.path_with_query().unwrap_or_else(|| "/".into());
        let path = path.split('?').next().unwrap_or("/").to_string();
        let method = req.method();

        let bucket = match open() {
            Ok(b) => b,
            Err(e) => return fail(out, 500, &format!("no store: {e}")),
        };

        match (&method, path.as_str()) {
            (Method::Get, "/") => respond(out, 200, "text/html; charset=utf-8", PAGE.as_bytes()),

            (Method::Get, "/health") => json_out(out, 200, &json!({ "ok": true })),

            // The prompt a vision provider should send, straight from the capability
            // that parses its output — so the two cannot drift.
            (Method::Get, "/api/prompt") => json_out(out, 200, &json!({ "prompt": ident::prompt() })),

            // A model's answer in, a card row out. The parse is the capability's; the
            // id and the storage are the app's.
            (Method::Post, "/api/scan") => {
                let body = read_body(&req);
                let answer = match serde_json::from_slice::<Value>(&body) {
                    Ok(v) => v.get("answer").and_then(Value::as_str).unwrap_or_default().to_string(),
                    Err(_) => String::from_utf8_lossy(&body).to_string(),
                };
                match ident::parse(&answer) {
                    Ok(g) => {
                        let id = format!(
                            "{}-{}",
                            if g.set_code.is_empty() { "unknown" } else { &g.set_code },
                            if g.number.is_empty() { g.name.replace(' ', "-").to_lowercase() } else { g.number.replace('/', "-") }
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
                            graded: g.graded.map(|gr| format!("{} {}", gr.grader, gr.tenths as f64 / 10.0)).unwrap_or_default(),
                            confidence: g.confidence,
                            // The capability calls the field `variant`; `variant` is a
                            // WIT keyword, so the contract calls it `printing` and so
                            // does this app. Renaming it here as well keeps the flag
                            // matching the field a screen is showing.
                            needs_review: g
                                .needs_review
                                .into_iter()
                                .map(|f| if f == "variant" { "printing".to_string() } else { f })
                                .collect(),
                        };
                        if let Err(e) = put_json(&bucket, &format!("card:{id}"), &card) {
                            return fail(out, 500, &e);
                        }
                        json_out(out, 201, &serde_json::to_value(card).unwrap_or(Value::Null))
                    }
                    // A refusal is a 422 and says why: not a card, several cards, or
                    // an answer with no name in it. None of them becomes a blank row.
                    Err(e) => fail(out, 422, &format!("{e:?}")),
                }
            }

            (Method::Get, "/api/cards") => {
                let cards: Vec<Card> = scan(&bucket, "card:");
                json_out(out, 200, &json!({ "cards": cards }))
            }

            // A correction. Only the fields sent are touched, and every one of them
            // leaves `needs_review` — that is what a person checking it means.
            (Method::Patch, "/api/cards") => {
                let body = read_body(&req);
                let Ok(patch) = serde_json::from_slice::<Value>(&body) else {
                    return fail(out, 400, "not json");
                };
                let Some(id) = patch.get("id").and_then(Value::as_str) else {
                    return fail(out, 400, "which card");
                };
                let Some(mut card) = get_json::<Card>(&bucket, &format!("card:{id}")) else {
                    return fail(out, 404, "no such card");
                };
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
                        *slot = v.to_string();
                        card.needs_review.retain(|r| r != field);
                    }
                }
                if let Err(e) = put_json(&bucket, &format!("card:{id}"), &card) {
                    return fail(out, 500, &e);
                }
                json_out(out, 200, &serde_json::to_value(card).unwrap_or(Value::Null))
            }

            // What you paid, or what you sold it for. A swap is two of these.
            (Method::Post, "/api/events") => {
                let body = read_body(&req);
                let Ok(e) = serde_json::from_slice::<Value>(&body) else {
                    return fail(out, 400, "not json");
                };
                let at = e.get("at").and_then(Value::as_u64).unwrap_or_else(now);
                let ev = StoredEvent {
                    card_id: e.get("card_id").and_then(Value::as_str).unwrap_or_default().to_string(),
                    kind: e.get("kind").and_then(Value::as_str).unwrap_or("acquired").to_string(),
                    quantity: e.get("quantity").and_then(Value::as_u64).unwrap_or(1) as u32,
                    unit_minor: e.get("unit_minor").and_then(Value::as_i64).unwrap_or(0),
                    currency: e.get("currency").and_then(Value::as_str).unwrap_or("EUR").to_string(),
                    at,
                };
                if ev.card_id.is_empty() {
                    return fail(out, 400, "which card");
                }
                // Keyed by instant and card, so replaying the same POST twice writes
                // one event rather than inventing a second purchase.
                let key = format!("event:{:020}:{}", at, ev.card_id);
                if let Err(e) = put_json(&bucket, &key, &ev) {
                    return fail(out, 500, &e);
                }
                json_out(out, 201, &serde_json::to_value(ev).unwrap_or(Value::Null))
            }

            // An observed price. Where it came from is not this app's business —
            // a market API, a scraper, or what the shop down the road is asking.
            (Method::Post, "/api/quotes") => {
                let body = read_body(&req);
                let Ok(q) = serde_json::from_slice::<Value>(&body) else {
                    return fail(out, 400, "not json");
                };
                let at = q.get("at").and_then(Value::as_u64).unwrap_or_else(now);
                let quote = StoredQuote {
                    card_id: q.get("card_id").and_then(Value::as_str).unwrap_or_default().to_string(),
                    unit_minor: q.get("unit_minor").and_then(Value::as_i64).unwrap_or(0),
                    currency: q.get("currency").and_then(Value::as_str).unwrap_or("EUR").to_string(),
                    at,
                };
                if quote.card_id.is_empty() {
                    return fail(out, 400, "which card");
                }
                let key = format!("quote:{}:{:020}", quote.card_id, at);
                if let Err(e) = put_json(&bucket, &key, &quote) {
                    return fail(out, 500, &e);
                }
                json_out(out, 201, &serde_json::to_value(quote).unwrap_or(Value::Null))
            }

            // One card's price over the last 90 days, carried across the gaps.
            (Method::Get, p) if p.starts_with("/api/price/") => {
                let card = p.trim_start_matches("/api/price/").to_string();
                let quotes: Vec<ph::Quote> = scan::<StoredQuote>(&bucket, &format!("quote:{card}:"))
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
                        out,
                        200,
                        &json!({
                            "card_id": card,
                            "points": points.iter().map(|p| json!({
                                "at": p.at, "unit_minor": p.unit_minor, "carried": p.carried
                            })).collect::<Vec<_>>()
                        }),
                    ),
                    Err(e) => fail(out, 422, &format!("{e:?}")),
                }
            }

            // What the whole collection is worth, and how it got there.
            (Method::Get, "/api/portfolio") => {
                let events = events_for(&bucket);
                let quotes = quotes_for(&bucket);
                let until = now();
                match pv::value_at(&events, &quotes, until) {
                    Ok(v) => {
                        let since = until.saturating_sub(90 * 86_400);
                        let points = pv::series(&events, &quotes, since, until, 86_400).unwrap_or_default();
                        json_out(
                            out,
                            200,
                            &json!({
                                "cost_basis_minor": v.cost_basis_minor,
                                "market_value_minor": v.market_value_minor,
                                "unrealised_minor": v.unrealised_minor,
                                "realised_minor": v.realised_minor,
                                "currency": v.currency,
                                "unquoted": v.unquoted,
                                "series": points.iter().map(|p| json!({
                                    "at": p.at,
                                    "market_value_minor": p.market_value_minor,
                                    "cost_basis_minor": p.cost_basis_minor,
                                    "realised_minor": p.realised_minor,
                                })).collect::<Vec<_>>()
                            }),
                        )
                    }
                    // An empty collection is not an error to a person looking at a
                    // screen, so it answers with zeroes and says the log is empty.
                    Err(pv::ValueError::Empty) => json_out(
                        out,
                        200,
                        &json!({
                            "cost_basis_minor": 0, "market_value_minor": 0,
                            "unrealised_minor": 0, "realised_minor": 0,
                            "currency": "EUR", "unquoted": 0, "series": [], "empty": true
                        }),
                    ),
                    // Every other one is a refusal the capability makes on purpose —
                    // mixed currency, an oversold card — and it is reported, not
                    // absorbed into a plausible number.
                    Err(e) => fail(out, 422, &format!("{e:?}")),
                }
            }

            _ => fail(out, 404, "no such route"),
        }
    }
}

bindings::export!(Component with_types_in bindings);
