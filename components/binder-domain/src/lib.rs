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
//! The UI is a React SPA in `examples/binder/ui`, served by the host from
//! `--static-dir` — this component answers `/api/*` and nothing else, so the two can
//! be developed and deployed apart. `just host-binder` builds and serves both.
//!
//! What is genuinely this component's is the HTTP surface and the storage. That is
//! the split ADR-0095 requires — the pieces meet through WIT, so each is testable on
//! its own, and each of those three is tested by a held-out specification that was
//! written before its implementation existed.
//!
//! ## Whose collection
//!
//! Everything except the prices belongs to somebody. `auth:identity` issues the
//! session and says who a bearer token is, and every key this app writes carries that
//! subject: `u/<subject>/card:<id>`. Two accounts on one deployment cannot see each
//! other's cards, and that is what makes selling one to another person a thing that
//! can exist later.
//!
//! Quotes are deliberately NOT per-account. What a card traded for is a fact about
//! the market, not about the owner, and giving every account its own copy would mean
//! two people holding the same card disagree about what it is worth.
//!
//! ## Storage
//!
//! Key spaces in one bucket, which the linker names after the app (ADR-0023):
//! `u/<subject>/card:<id>` a card, `u/<subject>/event:<at>:<card>` an acquisition or
//! disposal, `u/<subject>/deck:<name>` a deck list, and `quote:<card>:<at>` a price.
//! Lists are rebuilt by prefix scan rather than kept as an index, because a
//! collection is small and an index is a second thing to keep right.

#[allow(warnings)]
mod bindings;

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::types::Principal;
use bindings::card::identify::identifier as ident;
use bindings::deck::build::builder as deck;
use bindings::vision::describe::describer as vision;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::sheet::ingest::reader as sheet;
use bindings::portfolio::value::valuation as pv;
use bindings::price::history::history as ph;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use bindings::wasi::io::streams::StreamError;
use bindings::wasi::keyvalue::store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

struct Component;

// `"default"` is what every other domain in this repository opens, and the host
// names the bucket after the app regardless (ADR-0023) — the guest string does not
// choose the isolation boundary, which is the whole point of ADR-0015. An empty name
// is simply not a bucket the host has.
const BUCKET: &str = "default";

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

/// One correction to a card, kept.
///
/// A field edit overwrites the card, and that is right — the card is what it IS. But
/// "who said Near Mint, and when did that change" is a different question, and the
/// row alone cannot answer it. So every change is appended: what the field was, what
/// it became, and when. `from` empty means it was never established, which is the
/// ordinary case for a field the AI left flagged.
#[derive(Serialize, Deserialize, Clone)]
struct Change {
    card_id: String,
    field: String,
    from: String,
    to: String,
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

/// Store a guess as a card. Shared by `/api/scan` and `/api/photo` so a card that
/// arrived as a photograph and one that arrived as pasted text are the same row.
fn store_guess(b: &store::Bucket, ns: &str, g: ident::Guess) -> Result<Card, String> {
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
        // The capability calls the field `variant`; `variant` is a WIT keyword, so
        // the contract calls it `printing` and so does this app.
        needs_review: g
            .needs_review
            .into_iter()
            .map(|f| if f == "variant" { "printing".to_string() } else { f })
            .collect(),
    };
    put_json(b, &format!("{ns}card:{id}"), &card)?;
    Ok(card)
}

/// Decode base64, the half of it a `data:` URL from a browser produces.
///
/// Written out for the same reason the encoder in `anthropic-vision` is: this is the
/// whole of what is needed, and a crate for one alphabet is another thing to keep in
/// step. Whitespace is skipped because a data URL may arrive wrapped.
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

/// Percent-decode one path segment.
///
/// A deck called "some deck" is stored under that name and asked for as
/// `some%20deck`, so without this the two never meet: creating it works, opening it
/// is a 404, and the name looks right in both places. Written out rather than pulled
/// in — this is the whole of what the app needs, and a URL crate is a dependency for
/// six lines.
/// One `key=value` out of a query string, decoded.
///
/// Six lines rather than a URL crate, for the same reason as `percent_decode`: this
/// is the whole of what the app asks of a query string.
fn param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(v))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    // Not a valid escape: a literal `%` in a name, which is legal.
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// The key an event is stored under, which is its IDENTITY.
///
/// Everything that distinguishes one event from another is in it, and that is not
/// fussiness — the key was `event:{at}:{card}`, so buying a card and selling it in
/// the SAME SECOND wrote both to one key and the sale silently replaced the
/// purchase. The collection then held nothing, had no cost basis, and realised
/// nothing on a sale it still showed. Every number was wrong and none of them looked
/// it.
///
/// Replaying an identical POST still lands on the same key, which is the idempotency
/// the old scheme was reaching for; two DIFFERENT events at one instant no longer
/// collide.
fn event_key(ns: &str, e: &StoredEvent) -> String {
    format!(
        "{ns}event:{:020}:{}:{}:{}:{}",
        e.at, e.card_id, e.kind, e.quantity, e.unit_minor
    )
}

/// Every key under a prefix. `scan` reads values; deleting needs the keys.
fn keys_under(b: &store::Bucket, prefix: &str) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut cursor = None;
    loop {
        let Ok(page) = b.list_keys(cursor.clone()) else { break };
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

/// A deck list, as its owner saved it.
#[derive(Serialize, Deserialize, Clone)]
struct Deck {
    name: String,
    #[serde(default)]
    slots: Vec<DeckSlot>,
}

/// One line of a deck. `kind` is the format's word — `basic-pokemon`, `trainer`,
/// `basic-energy` — because the four-copy rule and the energy exemption are decided
/// by it and the collection does not otherwise care.
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
        // A card whose kind nobody set is a Basic Pokémon, which is the ONLY
        // conservative default: it is the one kind that is capped at four AND
        // satisfies the "needs a basic" rule, so a mislabelled card can never make an
        // illegal deck look legal.
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

// ---- the photo stream ---------------------------------------------------

/// Do the work and narrate it as Server-Sent Events.
///
/// One event per stage, each a JSON object with a `stage` — so a client renders
/// progress without parsing prose, and a stage added later does not break one that
/// only knows the old ones.
fn stream_photo(
    out: ResponseOutparam,
    bucket: &store::Bucket,
    ns: &str,
    bytes: Vec<u8>,
    media_type: String,
) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"text/event-stream".to_vec()]);
    let _ = headers.set(&"cache-control".to_string(), &[b"no-cache".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(200);
    let body = response.body().expect("outgoing body");
    // Set BEFORE the work starts, so the browser has the connection and the first
    // event arrives while the model is still looking rather than after it.
    ResponseOutparam::set(out, Ok(response));

    {
        let stream = body.write().expect("write stream");
        // Chunked, like every other write in this repository: one call above 4096
        // bytes traps the component mid-response, and an SSE frame carrying a card
        // with a long `needs_review` is not small.
        let send = |v: Value| -> bool {
            let frame = format!("data: {v}\n\n");
            for chunk in frame.as_bytes().chunks(4096) {
                if stream.blocking_write_and_flush(chunk).is_err() {
                    return false;
                }
            }
            true
        };

        if !send(json!({ "stage": "looking", "detail": "showing the card to the model" })) {
            return;
        }

        match vision::describe(&bytes, &media_type, &ident::prompt()) {
            Ok(answer) => {
                let _ = send(json!({ "stage": "reading", "detail": "reading the answer into fields" }));
                match ident::parse(&answer) {
                    Ok(g) => match store_guess(bucket, ns, g) {
                        Ok(card) => {
                            let _ = send(json!({
                                "stage": "done",
                                "card": serde_json::to_value(&card).unwrap_or(Value::Null),
                            }));
                        }
                        Err(e) => {
                            let _ = send(json!({ "stage": "failed", "error": e }));
                        }
                    },
                    // Not a card, several cards, or no name. The model's own words go
                    // with it: "that is a booster wrapper" is worth showing the person
                    // who took the photograph.
                    Err(e) => {
                        let _ = send(json!({
                            "stage": "refused",
                            "error": format!("{e:?}"),
                            "said": answer.chars().take(400).collect::<String>(),
                        }));
                    }
                }
            }
            // The provider's own words. A model that refused and a provider that is
            // down are different problems for the person holding the phone.
            Err(e) => {
                let _ = send(json!({ "stage": "failed", "error": format!("{e:?}") }));
            }
        }
    }
    let _ = OutgoingBody::finish(body, None);
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

/// The bearer token, or nothing. Only the one scheme: an app that also accepts a
/// token in a query string has a token in somebody's server log.
fn bearer(req: &IncomingRequest) -> Option<String> {
    req.headers()
        .get(&"authorization".to_string())
        .into_iter()
        .filter_map(|v| String::from_utf8(v).ok())
        .find_map(|v| v.strip_prefix("Bearer ").map(str::to_string))
}

/// Who is asking. Every route below except `/`, `/health` and the two auth routes
/// goes through here, and a route that forgets to is a route that reads somebody
/// else's collection.
fn who(req: &IncomingRequest) -> Option<Principal> {
    authorizer::introspect(&bearer(req)?).ok()
}

/// The key prefix for one account. Every read and write is scoped by this, so
/// isolation is a property of the key rather than of remembering to filter.
fn ns(p: &Principal) -> String {
    format!("u/{}/", p.subject)
}

/// A ceiling on what is read into memory, not a policy: past this the read gives up
/// rather than growing until the store's memory cap traps the component and the
/// connection simply closes. A photograph arrives here base64-encoded, so it is
/// generous — but bounded.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// The whole request body, or nothing.
///
/// `wasi:io` signals the end of a body with `Err(StreamError::Closed)` and a genuine
/// failure with the other arm. Collapsing both into `break` returns a TRUNCATED body
/// as if it were whole — for `/api/photo` that is half a picture described with
/// confidence, and for anything that parses it is a confusing 400. There is no error
/// channel out of this function, so a failed or over-long read returns EMPTY: a
/// caller that gets nothing writes nothing, and a caller handed a plausible prefix
/// stores it.
fn read_body(req: &IncomingRequest) -> Vec<u8> {
    let Ok(body) = req.consume() else { return Vec::new() };
    let Ok(stream) = body.stream() else { return Vec::new() };
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                if buf.len() + chunk.len() > MAX_BODY_BYTES {
                    return Vec::new();
                }
                buf.extend_from_slice(&chunk);
            }
            Err(StreamError::Closed) => break,
            // NOT the end of the body.
            Err(_) => return Vec::new(),
        }
    }
    buf
}

impl Guest for Component {
    fn handle(req: IncomingRequest, out: ResponseOutparam) {
        let full = req.path_with_query().unwrap_or_else(|| "/".into());
        let (path, query) = match full.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (full.clone(), String::new()),
        };
        let method = req.method();

        let bucket = match open() {
            Ok(b) => b,
            Err(e) => return fail(out, 500, &format!("no store: {e}")),
        };

        match (&method, path.as_str()) {
            (Method::Get, "/health") => json_out(out, 200, &json!({ "ok": true })),

            // The prompt a vision provider should send, straight from the capability
            // that parses its output — so the two cannot drift.
            (Method::Get, "/api/prompt") => json_out(out, 200, &json!({ "prompt": ident::prompt() })),

            // --- accounts ------------------------------------------------
            //
            // Thin on purpose: `auth:identity` owns hashing, sessions and
            // introspection, and this app owns none of it (ADR-0009).
            (Method::Post, "/api/register") => {
                let body = read_body(&req);
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    return fail(out, 400, "not json");
                };
                let email = v.get("email").and_then(Value::as_str).unwrap_or_default();
                let password = v.get("password").and_then(Value::as_str).unwrap_or_default();
                match accounts::register(email, password, "binder") {
                    Ok(p) => json_out(out, 201, &json!({ "subject": p.subject })),
                    Err(e) => fail(out, 409, &format!("{e:?}")),
                }
            }

            (Method::Post, "/api/login") => {
                let body = read_body(&req);
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    return fail(out, 400, "not json");
                };
                let email = v.get("email").and_then(Value::as_str).unwrap_or_default();
                let password = v.get("password").and_then(Value::as_str).unwrap_or_default();
                match accounts::login(email, password, "binder") {
                    Ok(t) => json_out(out, 200, &json!({ "access_token": t.access_token })),
                    // One message for every failure, so this cannot be used to find
                    // out which addresses have accounts.
                    Err(_) => fail(out, 401, "invalid credentials"),
                }
            }

            (Method::Get, "/api/me") => match who(&req) {
                Some(p) => json_out(out, 200, &json!({ "subject": p.subject, "roles": p.roles })),
                None => fail(out, 401, "sign in"),
            },

            // A PHOTOGRAPH in, a card row out. The whole point of the app: nobody
            // types a card in.
            //
            // ASYNC, in two halves. A vision call takes seconds to a minute, and a
            // POST that holds the connection open that long is a request the browser
            // may give up on, a proxy may cut, and a person cannot be told anything
            // about. So the upload only STORES the picture and answers immediately
            // with a job; the work happens on the event stream below, which can say
            // what it is doing while it does it.
            (Method::Post, "/api/photo") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let ns = ns(&who);
                let body = read_body(&req);
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    return fail(out, 400, "not json");
                };
                let media_type =
                    v.get("media_type").and_then(Value::as_str).unwrap_or("image/jpeg").to_string();
                let Some(data) = v.get("data").and_then(Value::as_str) else {
                    return fail(out, 400, "no image");
                };
                if decode_b64(data).is_none() {
                    // Checked HERE rather than on the stream: a picture that is not
                    // base64 is the caller's mistake and should be a 400 on the
                    // request that made it, not an error event a minute later.
                    return fail(out, 400, "the image is not base64");
                }
                // The instant, plus how long the payload is: unique per upload
                // without a random source, and the job is read back by exactly one
                // stream immediately afterwards.
                let id = format!("{}-{}", now(), data.len());
                let job = json!({ "media_type": media_type, "data": data });
                if let Err(e) = put_json(&bucket, &format!("{ns}job:{id}"), &job) {
                    return fail(out, 500, &e);
                }
                json_out(out, 202, &json!({ "job": id, "events": format!("/api/photo/{id}/events") }))
            }

            // The work, reported as it happens.
            //
            // The vision call runs INSIDE this request rather than in a background
            // worker, because a component instance is per-request (ADR-0037) and
            // there is nothing to run a job on afterwards. What the stream buys is
            // not parallelism — it is that the person watching is told `looking`,
            // then `reading`, then the answer, instead of a spinner and a timeout.
            (Method::Get, p) if p.starts_with("/api/photo/") && p.ends_with("/events") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let ns = ns(&who);
                let id = percent_decode(
                    p.trim_start_matches("/api/photo/").trim_end_matches("/events"),
                );
                let Some(job) = get_json::<Value>(&bucket, &format!("{ns}job:{id}")) else {
                    return fail(out, 404, "no such job");
                };
                // Claimed by deleting it: a stream that reconnects must not spend a
                // second vision call on the same picture.
                let _ = bucket.delete(&format!("{ns}job:{id}"));

                let media_type =
                    job.get("media_type").and_then(Value::as_str).unwrap_or("image/jpeg").to_string();
                let bytes = job
                    .get("data")
                    .and_then(Value::as_str)
                    .and_then(decode_b64)
                    .unwrap_or_default();

                stream_photo(out, &bucket, &ns, bytes, media_type)
            }

            // A model's answer in, a card row out. The parse is the capability's; the
            // id and the storage are the app's.
            (Method::Post, "/api/scan") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let ns = ns(&who);
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
                        if let Err(e) = put_json(&bucket, &format!("{ns}card:{id}"), &card) {
                            return fail(out, 500, &e);
                        }
                        json_out(out, 201, &serde_json::to_value(card).unwrap_or(Value::Null))
                    }
                    // A refusal is a 422 and says why: not a card, several cards, or
                    // an answer with no name in it. None of them becomes a blank row.
                    Err(e) => fail(out, 422, &format!("{e:?}")),
                }
            }

            // A card typed in by hand. NOT a scan: a person who typed the fields is
            // not guessing, so nothing is flagged for review and confidence is 100.
            // The two paths produce the same row, which is why `needs_review` is a
            // property of the row rather than of how it arrived.
            (Method::Post, "/api/cards") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let ns = ns(&who);
                let body = read_body(&req);
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    return fail(out, 400, "not json");
                };
                let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                let name = s("name");
                if name.trim().is_empty() {
                    return fail(out, 400, "a card needs a name");
                }
                let set_code = s("set_code").to_lowercase();
                let number = s("number");
                let id = format!(
                    "{}-{}",
                    if set_code.is_empty() { "unknown" } else { &set_code },
                    if number.is_empty() { name.replace(' ', "-").to_lowercase() } else { number.replace('/', "-") }
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
                    return fail(out, 500, &e);
                }
                // What you paid, if you said. Adding a card and recording its cost is
                // one action to a person, and making it two is how a collection ends
                // up with cards that have no basis and a chart that under-reports.
                if let Some(paid) = v.get("paid_minor").and_then(Value::as_i64) {
                    let at = v.get("at").and_then(Value::as_u64).unwrap_or_else(now);
                    let ev = StoredEvent {
                        card_id: id.clone(),
                        kind: "acquired".into(),
                        quantity: v.get("quantity").and_then(Value::as_u64).unwrap_or(1) as u32,
                        unit_minor: paid,
                        currency: v.get("currency").and_then(Value::as_str).unwrap_or("EUR").to_string(),
                        at,
                    };
                    let _ = put_json(&bucket, &event_key(&ns, &ev), &ev);
                }
                json_out(out, 201, &serde_json::to_value(card).unwrap_or(Value::Null))
            }

            // A collection that already exists somewhere else, arriving in one go.
            //
            // The body is the FILE, raw, and `?name=` carries its name — the name is
            // what selects the reader, because the bytes of a CSV are not reliably
            // distinguishable from any other text and guessing is how a
            // semicolon-delimited European export becomes one column.
            //
            // ALL OR NOTHING. Every row is validated before anything is written, and
            // one bad row writes nothing. A half-applied import is worse than a
            // refused one: the person cannot tell which half, and their second
            // attempt duplicates whatever the first got through.
            (Method::Post, "/api/cards/bulk") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let ns = ns(&who);
                let name = param(&query, "name").unwrap_or_else(|| "upload.csv".into());
                let body = read_body(&req);
                if body.is_empty() {
                    return fail(out, 400, "no file in the body");
                }

                let sheet = match sheet::read(&name, &body) {
                    Ok(s) => s,
                    Err(e) => return fail(out, 400, &format!("{e:?}")),
                };

                // Header names as a person writes them: `Paid (minor)`, `paid_minor`
                // and `PAID MINOR` are one column.
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
                    return fail(
                        out,
                        422,
                        &format!("no `name` column — found {:?}", sheet.header),
                    );
                }

                // Validate everything first. `row` is 1-based and counts the header,
                // so it is the row number the person sees in their spreadsheet.
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
                        // Minor units, integer. A price with a decimal point in it is
                        // a person writing euros where the column says cents, and
                        // silently rounding it is a wrong number on a chart.
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
                        // Typed by a person from a file they already had, so there is
                        // nothing for a model to be unsure about.
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
                            if c.is_empty() { "EUR".to_string() } else { c.to_uppercase() }
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
                        out,
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

                // Only now does anything change.
                let mut added = 0u32;
                let mut updated = 0u32;
                let mut priced = 0u32;
                for (card, event) in staged {
                    let k = format!("{ns}card:{}", card.id);
                    let existed = bucket.get(&k).ok().flatten().is_some();
                    if let Err(e) = put_json(&bucket, &k, &card) {
                        return fail(out, 500, &e);
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
                    out,
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

            // Gone, with whatever it was worth. A card removed from the collection
            // keeps its EVENTS: what you paid and what you sold it for is history, and
            // deleting the row must not silently rewrite a realised gain.
            (Method::Delete, "/api/cards") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let body = read_body(&req);
                let id = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_default();
                if id.is_empty() {
                    return fail(out, 400, "which card");
                }
                match bucket.delete(&format!("{}card:{id}", ns(&who))) {
                    Ok(()) => json_out(out, 200, &json!({ "deleted": id })),
                    Err(e) => fail(out, 500, &format!("{e:?}")),
                }
            }

            // One card, and how it got here: what it is, what is held, every
            // correction anyone made, and what it has been worth.
            (Method::Get, p) if p.starts_with("/api/cards/") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let ns = ns(&who);
                let id = percent_decode(p.trim_start_matches("/api/cards/"));
                let Some(card) = get_json::<Card>(&bucket, &format!("{ns}card:{id}")) else {
                    return fail(out, 404, "no such card");
                };

                let events: Vec<StoredEvent> = scan::<StoredEvent>(&bucket, &format!("{ns}event:"))
                    .into_iter()
                    .filter(|e| e.card_id == id)
                    .collect();
                let held: i64 = events
                    .iter()
                    .map(|e| if e.kind == "disposed" { -(e.quantity as i64) } else { e.quantity as i64 })
                    .sum();

                let stored: Vec<StoredQuote> = scan::<StoredQuote>(&bucket, &format!("quote:{id}:"));
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

                // The money comes from `portfolio:value`, over THIS card's events —
                // not from arithmetic here. Summing the purchases would report what
                // was ever spent rather than the basis of what is still held, so a
                // card sold at a profit would show a cost basis it no longer has and
                // disagree with the portfolio total that includes it. FIFO belongs to
                // the capability; the app's job is to hand it one card's log.
                let one_card: Vec<pv::Event> = events
                    .iter()
                    .map(|e| pv::Event {
                        item_id: e.card_id.clone(),
                        kind: if e.kind == "disposed" { pv::EventKind::Disposed } else { pv::EventKind::Acquired },
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
                // Newest first: a history is read from what just happened backwards.
                changes.sort_by(|a, b| b.at.cmp(&a.at));

                let mut evs = events;
                evs.sort_by(|a, b| b.at.cmp(&a.at));

                json_out(
                    out,
                    200,
                    &json!({
                        "card": card,
                        "held": held.max(0),
                        "cost_basis_minor": basis,
                        // What selling this card has already made, from the same FIFO
                        // the portfolio uses — so the card and the total agree.
                        "realised_minor": realised,
                        "price_minor": price.as_ref().map(|o| o.unit_minor),
                        "currency": price.as_ref().map(|o| o.currency.clone()).unwrap_or_else(|| "EUR".into()),
                        "price_age_days": price.as_ref().map(|o| o.age_seconds / 86_400),
                        "value_minor": price.as_ref().map(|o| held.max(0) * o.unit_minor),
                        // Each point says whether it was CARRIED, so a flat stretch
                        // reads as "nobody quoted it" rather than "it did not move".
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
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let ns = ns(&who);
                let cards: Vec<Card> = scan(&bucket, &format!("{ns}card:"));
                // Which decks each card is in. A card is not consumed by a deck, so
                // this is a list and not a flag — and seeing "in 3 decks" on a card
                // you are about to sell is the point.
                let decks: Vec<Deck> = scan(&bucket, &format!("{ns}deck:"));

                // How many are held, and what they cost. Both come from the SAME
                // event log the portfolio is valued from, so a row can never
                // disagree with the total above it.
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
                    out,
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

                            // The price is `price:history`'s answer, not the newest
                            // row: the same carry-forward rule the chart uses, so a
                            // card priced last week reads as last week's price rather
                            // than as unpriced.
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
                                    // How stale, and whether this is a carried price.
                                    // A four-month-old quote is the best information
                                    // there is and also barely information.
                                    v["price_age_days"] = json!(o.age_seconds / 86_400);
                                    v["value_minor"] = json!(qty.max(0) * o.unit_minor);
                                }
                                // Absent, not zero. A card nothing has priced is not
                                // worth nothing, and the row says so.
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

            // A correction. Only the fields sent are touched, and every one of them
            // leaves `needs_review` — that is what a person checking it means.
            (Method::Patch, "/api/cards") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let ns = ns(&who);
                let body = read_body(&req);
                let Ok(patch) = serde_json::from_slice::<Value>(&body) else {
                    return fail(out, 400, "not json");
                };
                let Some(id) = patch.get("id").and_then(Value::as_str) else {
                    return fail(out, 400, "which card");
                };
                let Some(mut card) = get_json::<Card>(&bucket, &format!("{ns}card:{id}")) else {
                    return fail(out, 404, "no such card");
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
                        // Only an actual change. Saving a row without touching a
                        // field would otherwise write a history entry saying it
                        // became what it already was.
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
                    // Keyed by FIELD, not by an index within this save: an index
                    // restarts at zero on the next save, so correcting two fields and
                    // then one more in the same second silently overwrote the first
                    // one's history. A field can only meaningfully change once in a
                    // second anyway.
                    let _ = put_json(
                        &bucket,
                        &format!("{ns}change:{at:020}:{id}:{}", c.field),
                        c,
                    );
                }
                if let Err(e) = put_json(&bucket, &format!("{ns}card:{id}"), &card) {
                    return fail(out, 500, &e);
                }
                json_out(out, 200, &serde_json::to_value(card).unwrap_or(Value::Null))
            }

            // What you paid, or what you sold it for. A swap is two of these.
            (Method::Post, "/api/events") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let ns = ns(&who);
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
                // A disposal of more than is held is refused HERE, on the request that
                // makes it, rather than by the valuation later.
                //
                // `portfolio:value` refuses an oversold log by design and is right to:
                // guessing which sale was wrong is a bigger lie than refusing. But the
                // refusal lands on `/api/portfolio`, which means one bad event takes
                // out every screen at once and the person is left with a 422 and no
                // way back. The place to catch it is the write.
                if ev.kind == "disposed" {
                    let held: i64 = scan::<StoredEvent>(&bucket, &format!("{ns}event:"))
                        .into_iter()
                        .filter(|e| e.card_id == ev.card_id && e.at <= at)
                        .map(|e| if e.kind == "disposed" { -(e.quantity as i64) } else { e.quantity as i64 })
                        .sum();
                    if (ev.quantity as i64) > held {
                        return fail(
                            out,
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
                    return fail(out, 500, &e);
                }
                json_out(out, 201, &serde_json::to_value(ev).unwrap_or(Value::Null))
            }

            // Remove one. The only way back from a log that cannot be valued, and
            // the reason the refusal above names an `at`.
            (Method::Delete, "/api/events") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let body = read_body(&req);
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    return fail(out, 400, "not json");
                };
                let (Some(card), Some(at)) = (
                    v.get("card_id").and_then(Value::as_str),
                    v.get("at").and_then(Value::as_u64),
                ) else {
                    return fail(out, 400, "which event — card_id and at");
                };
                // The EXACT event when the caller names it fully, which the card page
                // does because it is showing the thing being deleted. A prefix on
                // (instant, card) alone removes a buy and a sell that share a second —
                // exactly the pair that used to collide, so deleting the sale took the
                // purchase with it.
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
                        // Only if it is really there, so the count does not claim a
                        // deletion that did not happen.
                        if bucket.exists(&key).unwrap_or(false) { vec![key] } else { vec![] }
                    }
                    // Named only by instant and card: every event that matches goes,
                    // and the count says how many so a caller is not surprised.
                    _ => keys_under(&bucket, &format!("{ns}event:{:020}:{card}:", at)),
                };
                for k in &found {
                    let _ = bucket.delete(k);
                }
                json_out(
                    out,
                    200,
                    &json!({ "deleted": found.len(), "card_id": card, "at": at }),
                )
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
                let card = percent_decode(p.trim_start_matches("/api/price/"));
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
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let events = events_for(&bucket, &ns(&who));
                let quotes = quotes_for(&bucket);
                let until = now();
                // The window is the CALLER's, because a range selector that only
                // slices what the server already sent cannot show anything older
                // than the default — and "all" is a real answer that needs the
                // earliest event to compute.
                let days = param(&query, "days").and_then(|d| d.parse::<u64>().ok());
                let earliest = events.iter().map(|e| e.at).min().unwrap_or(until);
                let window = match days {
                    // `days=0` means everything, from the first thing that happened.
                    Some(0) => until.saturating_sub(earliest),
                    Some(d) => d * 86_400,
                    None => 90 * 86_400,
                };
                // One sample per day up to a quarter, then coarser: a five-year
                // series at daily resolution is 1800 points nobody can see, and the
                // step is what the caller is really choosing.
                let step = param(&query, "step")
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|s| *s > 0)
                    .unwrap_or_else(|| if window > 400 * 86_400 { 7 * 86_400 } else { 86_400 });
                match pv::value_at(&events, &quotes, until) {
                    Ok(v) => {
                        let since = until.saturating_sub(window.max(step));
                        let points = pv::series(&events, &quotes, since, until, step).unwrap_or_default();
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
                                "since": until.saturating_sub(window.max(step)),
                                "until": until,
                                "step": step,
                                // So a range selector can offer "all" without
                                // guessing how far back there is anything to show.
                                "earliest_event": earliest,
                                // Every field the valuation computed for that
                                // instant, not just the height of the line: a point
                                // you can hover needs the numbers behind the pixel,
                                // and so does anything else reading this route.
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
                    // mixed currency, an oversold card. Reported, never absorbed into
                    // a plausible number, and NOT as a 422 for the whole page:
                    // a log with one bad event in it used to take out the portfolio,
                    // the cards and the decks at once, leaving no screen from which to
                    // fix the event. So: 200, zeroes, and the card to go and look at.
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
                            out,
                            200,
                            &json!({
                                "cost_basis_minor": 0, "market_value_minor": 0,
                                "unrealised_minor": 0, "realised_minor": 0,
                                "currency": "EUR", "unquoted": 0, "series": [],
                                // Named so a screen can say what to fix and where.
                                "blocked": problem,
                                "blocked_card": card,
                            }),
                        )
                    }
                }
            }

            // --- decks ---------------------------------------------------
            //
            // The legality verdict and the shopping list are `deck:build`'s; this
            // route owns which deck, whose collection, and which prices.
            (Method::Get, "/api/decks") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let decks: Vec<Deck> = scan(&bucket, &format!("{}deck:", ns(&who)));
                json_out(out, 200, &json!({ "decks": decks }))
            }

            // Create an empty deck. A deck you are about to fill has to exist
            // before you can put anything in it, and PUT-with-slots cannot express
            // "new and empty" without pretending the list is already right.
            (Method::Post, "/api/decks") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let body = read_body(&req);
                let name = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_default();
                if name.trim().is_empty() {
                    return fail(out, 400, "a deck needs a name");
                }
                let key = format!("{}deck:{name}", ns(&who));
                if get_json::<Deck>(&bucket, &key).is_some() {
                    return fail(out, 409, "you already have a deck by that name");
                }
                let d = Deck { name, slots: vec![] };
                if let Err(e) = put_json(&bucket, &key, &d) {
                    return fail(out, 500, &e);
                }
                json_out(out, 201, &serde_json::to_value(d).unwrap_or(Value::Null))
            }

            // Add a card to a deck, or change how many. A card is NOT consumed by
            // this: the collection is what you own and a deck is a list that refers
            // to it, so one card can be in as many decks as you like.
            (Method::Post, p) if p.starts_with("/api/decks/") && p.ends_with("/slots") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let ns = ns(&who);
                let name = percent_decode(p.trim_start_matches("/api/decks/").trim_end_matches("/slots"));
                let key = format!("{ns}deck:{name}");
                let Some(mut d) = get_json::<Deck>(&bucket, &key) else {
                    return fail(out, 404, "no such deck");
                };
                let body = read_body(&req);
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    return fail(out, 400, "not json");
                };
                let card_id = v.get("card_id").and_then(Value::as_str).unwrap_or_default().to_string();
                if card_id.is_empty() {
                    return fail(out, 400, "which card");
                }
                // The printed name comes from the COLLECTION when it is there, so a
                // deck cannot disagree with the card about what it is called — and
                // the four-copy rule counts names.
                let held = get_json::<Card>(&bucket, &format!("{ns}card:{card_id}"));
                let card_name = v
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| held.as_ref().map(|c| c.name.clone()))
                    .unwrap_or_else(|| card_id.clone());
                let quantity = v.get("quantity").and_then(Value::as_u64).unwrap_or(1) as u32;
                let kind = v.get("kind").and_then(Value::as_str).unwrap_or("basic-pokemon").to_string();

                d.slots.retain(|s| s.card_id != card_id);
                // Zero removes it rather than storing an empty slot — which the
                // legality check would report as a typo, correctly.
                if quantity > 0 {
                    d.slots.push(DeckSlot { card_id, name: card_name, kind, quantity });
                }
                d.slots.sort_by(|a, b| a.card_id.cmp(&b.card_id));
                if let Err(e) = put_json(&bucket, &key, &d) {
                    return fail(out, 500, &e);
                }
                json_out(out, 200, &serde_json::to_value(d).unwrap_or(Value::Null))
            }

            (Method::Delete, "/api/decks") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let body = read_body(&req);
                let name = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_default();
                if name.is_empty() {
                    return fail(out, 400, "which deck");
                }
                // Only the list goes. The cards it referred to are still yours —
                // that is the whole difference between a deck and a box.
                match bucket.delete(&format!("{}deck:{name}", ns(&who))) {
                    Ok(()) => json_out(out, 200, &json!({ "deleted": name })),
                    Err(e) => fail(out, 500, &format!("{e:?}")),
                }
            }

            (Method::Put, "/api/decks") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let body = read_body(&req);
                let Ok(d) = serde_json::from_slice::<Deck>(&body) else {
                    return fail(out, 400, "not a deck");
                };
                if d.name.is_empty() {
                    return fail(out, 400, "a deck needs a name");
                }
                if let Err(e) = put_json(&bucket, &format!("{}deck:{}", ns(&who), d.name), &d) {
                    return fail(out, 500, &e);
                }
                json_out(out, 200, &serde_json::to_value(d).unwrap_or(Value::Null))
            }

            // Is it legal, and what would finishing it cost? Both at once, because
            // they are the two questions a builder asks about the same list and
            // answering them in two round trips is two chances to disagree.
            (Method::Get, p) if p.starts_with("/api/decks/") => {
                let Some(who) = who(&req) else { return fail(out, 401, "sign in") };
                let ns = ns(&who);
                let name = percent_decode(p.trim_start_matches("/api/decks/"));
                let Some(d) = get_json::<Deck>(&bucket, &format!("{ns}deck:{name}")) else {
                    return fail(out, 404, "no such deck");
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

                // What this account holds, from the same event log the portfolio is
                // valued from — so "owned" cannot drift from "what you paid for".
                let mut held: std::collections::BTreeMap<String, i64> = Default::default();
                for e in scan::<StoredEvent>(&bucket, &format!("{ns}event:")) {
                    let n = e.quantity as i64;
                    *held.entry(e.card_id).or_insert(0) += if e.kind == "disposed" { -n } else { n };
                }
                let owned: Vec<deck::Owned> = held
                    .into_iter()
                    .filter(|(_, n)| *n > 0)
                    .map(|(card_id, n)| deck::Owned { card_id, quantity: n as u32 })
                    .collect();

                // The newest quote per card, which is what a shopping list is priced
                // at — not an average, and not the first one found.
                let mut newest: std::collections::BTreeMap<String, StoredQuote> = Default::default();
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
                    out,
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
                        // Named, because a shopping list that quietly omits the
                        // unpriced cards is a total that is too low.
                        "unpriced": short.unpriced,
                    }),
                )
            }

            _ => fail(out, 404, "no such route"),
        }
    }
}

bindings::export!(Component with_types_in bindings);
