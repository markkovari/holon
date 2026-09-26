//! Timed competitions (CONTRACT.md "Timed competitions", "Game routes").
//!
//! Curators create, edit, publish, archive and judge; photographers list, read,
//! enter, vote, and read the leaderboard and the results.
//!
//! Collections this module writes:
//!
//! * `competitions` — the contract's record, indexed by `state`. Once results are
//!   frozen the record also carries `results` (below).
//! * `competition_entries` — `{competition, photo, owner, sha256, entered_at,
//!   display_name, auto, auto_version, auto_flags, verdict}`, indexed by
//!   `competition`, `photo`, `owner`, `sha256`. `moderation.rs` reads it by `photo`
//!   to decide a photo is on a shared surface (reportable).
//! * `competition_votes` — `{competition, entry, voter, stars, at}`.
//! * `competition_judgements` — `{competition, entry, curator, score, note, at}`.
//!
//! **auto-v1 is computed once, at entry**, and stored with `auto_version`, so a
//! later change to the formula in `rules.rs` never re-ranks a competition.
//!
//! **Results are frozen once.** The first read after `judging_closes_at` computes
//! the ranking and writes it into the competition record under a revision check;
//! a concurrent reader that loses that race re-reads and serves the winner's
//! ranking. Prize XP goes through the same guard: every prize is *claimed* by a
//! revision-checked write before `progress::credit` is called for it, so however
//! many reads race, one of them credits a given place, once.

use crate::bindings::auth::identity::types::Principal;
use crate::bindings::media::pipeline::jobs as media;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::clock;
use crate::moderation::{is_hidden, require_active, require_role};
use crate::{audit, introspect, Reply, Route};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

const COMPETITIONS: &str = "competitions";
const ENTRIES: &str = "competition_entries";
const VOTES: &str = "competition_votes";
const JUDGEMENTS: &str = "competition_judgements";
const PHOTOS: &str = "photos";
const JOURNEYS: &str = "journeys";
const ACCOUNTS: &str = "accounts";
const SIGN_TTL_SECS: u32 = 3600;
const AUTO_VERSION: &str = "auto-v1";
const MAX_NOTE: usize = 2000;

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        // ---- curator ----
        (Method::Get, ["api", "curator", "competitions"]) => curator_list(&principal),
        (Method::Post, ["api", "curator", "competitions"]) => create(&principal, body),
        (Method::Get, ["api", "curator", "competitions", id]) => curator_get(&principal, id),
        (Method::Put, ["api", "curator", "competitions", id]) => edit(&principal, id, body),
        (Method::Post, ["api", "curator", "competitions", id, "publish"]) => {
            transition(&principal, id, "publish")
        }
        (Method::Post, ["api", "curator", "competitions", id, "archive"]) => {
            transition(&principal, id, "archive")
        }
        (Method::Put, ["api", "curator", "competitions", id, "entries", entry, "judge"]) => {
            judge(&principal, id, entry, body)
        }
        // ---- photographer ----
        (Method::Get, ["api", "competitions"]) => list(&principal),
        (Method::Get, ["api", "competitions", id]) => detail(&principal, id),
        (Method::Get, ["api", "competitions", id, "leaderboard"]) => leaderboard(&principal, id),
        (Method::Get, ["api", "competitions", id, "results"]) => results(&principal, id),
        (Method::Post, ["api", "competitions", id, "entries"]) => enter(&principal, id, body),
        (Method::Put, ["api", "competitions", id, "entries", entry, "vote"]) => {
            vote(&principal, id, entry, body)
        }
        _ => Reply::err(404, "not_found"),
    }
}

// ---------------------------------------------------------------------------
// record helpers

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn doc(entry: &records::Entry) -> Map<String, Value> {
    let mut m = match serde_json::from_str::<Value>(&entry.data) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    m.insert("id".into(), json!(entry.id));
    m
}

fn data_of(m: &Map<String, Value>) -> String {
    let mut d = m.clone();
    d.remove("id");
    Value::Object(d).to_string()
}

fn str_of<'a>(m: &'a Map<String, Value>, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn u64_of(m: &Map<String, Value>, key: &str) -> u64 {
    m.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn f64_of(m: &Map<String, Value>, key: &str) -> f64 {
    m.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

type EntryDoc = (records::Entry, Map<String, Value>);

fn load(collection: &str, id: &str) -> Result<Option<EntryDoc>, Reply> {
    if !valid_id(id) {
        return Ok(None);
    }
    match records::get(collection, id) {
        Ok(e) => {
            let m = doc(&e);
            Ok(Some((e, m)))
        }
        Err(records::StoreError::NotFound) => Ok(None),
        Err(_) => Err(Reply::err(500, "store_error")),
    }
}

fn find(collection: &str, field: &str, value: &str) -> Result<Vec<Map<String, Value>>, Reply> {
    let v = serde_json::to_string(value).unwrap_or_default();
    match records::find_by(collection, field, &v) {
        Ok(es) => Ok(es.iter().map(doc).collect()),
        Err(records::StoreError::NotFound) => Ok(Vec::new()),
        Err(_) => Err(Reply::err(500, "store_error")),
    }
}

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(r) => return r,
        }
    };
}

/// A competition a photographer may read: published, or archived (read-only history).
fn public_competition(id: &str) -> Result<(records::Entry, Map<String, Value>), Reply> {
    match load(COMPETITIONS, id)? {
        Some((e, m)) if matches!(str_of(&m, "state"), "published" | "archived") => Ok((e, m)),
        _ => Err(Reply::err(404, "not_found")),
    }
}

/// The public shape of an entrant: never the account subject or the full email,
/// only a display name — the local part of the email moderation.rs keeps in
/// `accounts` (`{subject, email}`), or, for an account registered before that
/// book existed, a name derived from the subject.
fn display_name(subject: &str) -> String {
    let email = find(ACCOUNTS, "subject", subject)
        .ok()
        .and_then(|a| a.into_iter().next())
        .map(|a| str_of(&a, "email").to_string())
        .unwrap_or_default();
    name_from(subject, &email)
}

fn name_from(subject: &str, email: &str) -> String {
    let local = email.split('@').next().unwrap_or_default().trim();
    if !local.is_empty() {
        return local.to_string();
    }
    let tail: String =
        subject.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
    format!("photographer-{tail}")
}

fn thumb_url(photo: &Map<String, Value>) -> Value {
    let key = photo
        .get("renditions")
        .and_then(|r| r.get("thumb"))
        .and_then(|r| r.get("key"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if key.is_empty() {
        return Value::Null;
    }
    media::sign_get(key, SIGN_TTL_SECS).map(Value::String).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// validation (pure)

#[derive(Clone, Copy, Debug, PartialEq)]
struct Weights {
    auto: f64,
    votes: f64,
    judges: f64,
}

fn parse_weights(v: &Value) -> Result<Weights, String> {
    let Some(o) = v.as_object() else {
        return Err("weights must be an object {auto, votes, judges}".into());
    };
    for k in o.keys() {
        if !matches!(k.as_str(), "auto" | "votes" | "judges") {
            return Err(format!("unknown weight {k:?}"));
        }
    }
    let get = |k: &str| -> Result<f64, String> {
        match o.get(k) {
            None => Ok(0.0),
            Some(x) => match x.as_f64() {
                Some(f) if f.is_finite() && f >= 0.0 => Ok(f),
                _ => Err(format!("weights.{k} must be a number ≥ 0")),
            },
        }
    };
    let w = Weights { auto: get("auto")?, votes: get("votes")?, judges: get("judges")? };
    let sum = w.auto + w.votes + w.judges;
    if (sum - 1.0).abs() > 1e-6 {
        return Err(format!("weights sum to {sum}, not 1"));
    }
    Ok(w)
}

fn weights_of(c: &Map<String, Value>) -> Weights {
    c.get("weights").and_then(|w| parse_weights(w).ok()).unwrap_or(Weights {
        auto: 1.0,
        votes: 0.0,
        judges: 0.0,
    })
}

/// `opens_at < closes_at ≤ voting_closes_at ≤ judging_closes_at`. Judging must
/// outlast voting because results freeze at `judging_closes_at`: a vote after
/// that would change a ranking that has already paid out.
fn check_windows(opens: u64, closes: u64, voting: u64, judging: u64) -> Result<(), String> {
    if opens >= closes {
        return Err("opens_at must be before closes_at".into());
    }
    if voting < closes {
        return Err("voting_closes_at must not be before closes_at".into());
    }
    if judging < voting {
        return Err("judging_closes_at must not be before voting_closes_at".into());
    }
    Ok(())
}

fn bad(code: &str, detail: impl Into<String>) -> Reply {
    Reply::json(400, json!({"error": code, "detail": detail.into()}))
}

/// Validate a whole competition record (after a create or a merged edit).
fn validate(c: &Map<String, Value>) -> Result<(), Reply> {
    if str_of(c, "title").trim().is_empty() {
        return Err(Reply::err(400, "title is required"));
    }
    let w = c.get("weights").ok_or_else(|| bad("bad_weights", "weights is required"))?;
    parse_weights(w).map_err(|d| bad("bad_weights", d))?;
    let mut t = [0u64; 4];
    for (i, k) in
        ["opens_at", "closes_at", "voting_closes_at", "judging_closes_at"].iter().enumerate()
    {
        t[i] = match c.get(*k).and_then(Value::as_u64) {
            Some(v) => v,
            None => return Err(bad("bad_windows", format!("{k} is required (unix seconds)"))),
        };
    }
    check_windows(t[0], t[1], t[2], t[3]).map_err(|d| bad("bad_windows", d))?;
    let req = c.get("requirements").cloned().unwrap_or_else(|| json!({}));
    if !req.is_object() {
        return Err(bad("bad_requirements", "requirements must be an object"));
    }
    crate::rules::validate(&req).map_err(|d| bad("bad_requirements", d))?;
    match c.get("max_entries_per_user").and_then(Value::as_u64) {
        Some(n) if n >= 1 => {}
        _ => return Err(Reply::err(400, "max_entries_per_user must be an integer ≥ 1")),
    }
    match c.get("prizes_xp") {
        Some(Value::Array(a)) if a.iter().all(|x| x.as_u64().is_some()) => {}
        _ => return Err(Reply::err(400, "prizes_xp must be an array of non-negative integers")),
    }
    match c.get("journey") {
        None | Some(Value::Null) => {}
        Some(Value::String(j)) => {
            if load(JOURNEYS, j)?.is_none() {
                return Err(bad("bad_journey", format!("no journey {j}")));
            }
        }
        Some(_) => return Err(bad("bad_journey", "journey must be a journey id or null")),
    }
    Ok(())
}

/// Fields a curator may set. Anything else in a body is ignored.
const EDITABLE: &[&str] = &[
    "title",
    "brief",
    "requirements",
    "opens_at",
    "closes_at",
    "voting_closes_at",
    "judging_closes_at",
    "weights",
    "max_entries_per_user",
    "prizes_xp",
    "journey",
];
/// Fields that may still change once published: wording only. Everything else is
/// the rules entrants agreed to.
const EDITABLE_PUBLISHED: &[&str] = &["title", "brief"];

// ---------------------------------------------------------------------------
// curator routes

fn curator(principal: &Principal) -> Result<(), Reply> {
    require_role(principal, "curator")
}

fn curator_list(principal: &Principal) -> Reply {
    tri!(curator(principal));
    let mut out = Vec::new();
    let mut after = String::new();
    loop {
        let page = match records::list_records(COMPETITIONS, 200, &after) {
            Ok(p) => p,
            Err(records::StoreError::NotFound) => break,
            Err(_) => return Reply::err(500, "store_error"),
        };
        out.extend(page.entries.iter().map(|e| Value::Object(doc(e))));
        if page.next.is_empty() {
            break;
        }
        after = page.next;
    }
    out.reverse();
    Reply::json(200, json!({"competitions": out}))
}

fn curator_get(principal: &Principal, id: &str) -> Reply {
    tri!(curator(principal));
    match tri!(load(COMPETITIONS, id)) {
        Some((_, m)) => Reply::json(200, Value::Object(m)),
        None => Reply::err(404, "not_found"),
    }
}

fn create(principal: &Principal, body: &str) -> Reply {
    tri!(curator(principal));
    tri!(require_active(principal));
    let Ok(Value::Object(req)) = serde_json::from_str::<Value>(body) else {
        return Reply::err(400, "bad_json");
    };
    let mut c = Map::new();
    for k in EDITABLE {
        if let Some(v) = req.get(*k) {
            c.insert((*k).to_string(), v.clone());
        }
    }
    c.entry("brief").or_insert(json!(""));
    c.entry("requirements").or_insert(json!({}));
    c.entry("max_entries_per_user").or_insert(json!(1));
    c.entry("prizes_xp").or_insert(json!([]));
    c.entry("journey").or_insert(Value::Null);
    c.insert("state".into(), json!("draft"));
    c.insert("created_by".into(), json!(principal.subject));
    c.insert("created_at".into(), json!(clock::now()));
    tri!(validate(&c));
    let entry = match records::create(COMPETITIONS, &data_of(&c), &["state".to_string()]) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_error"),
    };
    audit("competition.create", "allow", &principal.subject, &entry.id);
    Reply::json(201, Value::Object(doc(&entry)))
}

fn edit(principal: &Principal, id: &str, body: &str) -> Reply {
    tri!(curator(principal));
    tri!(require_active(principal));
    let Ok(Value::Object(req)) = serde_json::from_str::<Value>(body) else {
        return Reply::err(400, "bad_json");
    };
    for _ in 0..3 {
        let Some((entry, mut c)) = tri!(load(COMPETITIONS, id)) else {
            return Reply::err(404, "not_found");
        };
        let state = str_of(&c, "state").to_string();
        if state == "archived" {
            return Reply::err(409, "competition_archived");
        }
        for k in EDITABLE {
            let Some(v) = req.get(*k) else { continue };
            if state == "published" && !EDITABLE_PUBLISHED.contains(k) && c.get(*k) != Some(v) {
                return Reply::json(
                    409,
                    json!({"error": "competition_published",
                           "detail": format!("{k} cannot change once published; archive and replace it")}),
                );
            }
            c.insert((*k).to_string(), v.clone());
        }
        tri!(validate(&c));
        c.insert("updated_at".into(), json!(clock::now()));
        match records::update(COMPETITIONS, id, &data_of(&c), entry.revision) {
            Ok(e) => {
                audit("competition.edit", "allow", &principal.subject, id);
                return Reply::json(200, Value::Object(doc(&e)));
            }
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(_) => return Reply::err(500, "store_error"),
        }
    }
    Reply::err(503, "busy")
}

fn transition(principal: &Principal, id: &str, action: &str) -> Reply {
    tri!(curator(principal));
    tri!(require_active(principal));
    for _ in 0..3 {
        let Some((entry, mut c)) = tri!(load(COMPETITIONS, id)) else {
            return Reply::err(404, "not_found");
        };
        let state = str_of(&c, "state").to_string();
        let target = match (action, state.as_str()) {
            ("publish", "published") | ("archive", "archived") => {
                return Reply::json(200, Value::Object(c));
            }
            ("publish", "archived") => return Reply::err(409, "competition_archived"),
            ("publish", _) => "published",
            _ => "archived",
        };
        if target == "published" {
            tri!(validate(&c));
            c.insert("published_at".into(), json!(clock::now()));
        } else {
            c.insert("archived_at".into(), json!(clock::now()));
        }
        c.insert("state".into(), json!(target));
        match records::update(COMPETITIONS, id, &data_of(&c), entry.revision) {
            Ok(e) => {
                audit(&format!("competition.{action}"), "allow", &principal.subject, id);
                return Reply::json(200, Value::Object(doc(&e)));
            }
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(_) => return Reply::err(500, "store_error"),
        }
    }
    Reply::err(503, "busy")
}

fn judge(principal: &Principal, id: &str, entry_id: &str, body: &str) -> Reply {
    tri!(curator(principal));
    tri!(require_active(principal));
    let Ok(Value::Object(req)) = serde_json::from_str::<Value>(body) else {
        return Reply::err(400, "bad_json");
    };
    let score = match req.get("score").and_then(Value::as_f64) {
        Some(s) if s.is_finite() && (0.0..=10.0).contains(&s) => s,
        _ => return Reply::err(400, "score must be a number 0..10"),
    };
    let note = match req.get("note") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) if s.len() <= MAX_NOTE => s.clone(),
        Some(_) => return Reply::err(400, "note must be a string of at most 2000 bytes"),
    };
    let Some((_, c)) = tri!(load(COMPETITIONS, id)) else {
        return Reply::err(404, "not_found");
    };
    if str_of(&c, "state") != "published" {
        return Reply::err(404, "not_found");
    }
    let (_, e) = tri!(entry_of(id, entry_id));
    if clock::now() >= u64_of(&c, "judging_closes_at") {
        return Reply::err(409, "judging_closed");
    }
    if let Some((_, p)) = tri!(load(PHOTOS, str_of(&e, "photo"))) {
        if is_hidden(&p) {
            return Reply::err(409, "photo_hidden");
        }
    }
    let rec = json!({
        "competition": id, "entry": entry_id, "curator": principal.subject,
        "score": score, "note": note, "at": clock::now(),
    });
    tri!(upsert(JUDGEMENTS, entry_id, "curator", &principal.subject, rec));
    audit("competition.judge", "allow", &principal.subject, entry_id);
    Reply::json(200, json!({"entry": entry_id, "score": score, "note": note}))
}

/// One record per (`who_field` = `who`, entry): update it if it exists, else create it.
fn upsert(
    collection: &str,
    entry_id: &str,
    who_field: &str,
    who: &str,
    rec: Value,
) -> Result<(), Reply> {
    let v = serde_json::to_string(entry_id).unwrap_or_default();
    for _ in 0..3 {
        let existing = match records::find_by(collection, "entry", &v) {
            Ok(es) => es,
            Err(records::StoreError::NotFound) => Vec::new(),
            Err(_) => return Err(Reply::err(500, "store_error")),
        };
        let mine = existing.iter().find(|e| str_of(&doc(e), who_field) == who);
        let r = match mine {
            Some(e) => records::update(collection, &e.id, &rec.to_string(), e.revision).map(|_| ()),
            None => records::create(
                collection,
                &rec.to_string(),
                &["entry".to_string(), "competition".to_string()],
            )
            .map(|_| ()),
        };
        match r {
            Ok(()) => return Ok(()),
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(_) => return Err(Reply::err(500, "store_error")),
        }
    }
    Err(Reply::err(503, "busy"))
}

fn entry_of(
    competition: &str,
    entry_id: &str,
) -> Result<(records::Entry, Map<String, Value>), Reply> {
    match load(ENTRIES, entry_id)? {
        Some((e, m)) if str_of(&m, "competition") == competition => Ok((e, m)),
        _ => Err(Reply::err(404, "not_found")),
    }
}

// ---------------------------------------------------------------------------
// photographer routes

fn phase(c: &Map<String, Value>, now: u64) -> &'static str {
    if str_of(c, "state") == "archived" {
        return "archived";
    }
    if now < u64_of(c, "opens_at") {
        "upcoming"
    } else if now < u64_of(c, "closes_at") {
        "open"
    } else if now < u64_of(c, "voting_closes_at") {
        "voting"
    } else if now < u64_of(c, "judging_closes_at") {
        "judging"
    } else {
        "finished"
    }
}

/// The public view of a competition record: no frozen results blob, plus `phase`.
fn public_view(c: &Map<String, Value>, now: u64) -> Map<String, Value> {
    let mut v = c.clone();
    v.remove("results");
    v.insert("phase".into(), json!(phase(c, now)));
    v
}

fn list(_principal: &Principal) -> Reply {
    let now = clock::now();
    let mut out = tri!(find(COMPETITIONS, "state", "published"));
    out.sort_by(|a, b| {
        u64_of(b, "opens_at")
            .cmp(&u64_of(a, "opens_at"))
            .then_with(|| str_of(b, "id").cmp(str_of(a, "id")))
    });
    let out: Vec<Value> = out.iter().map(|c| Value::Object(public_view(c, now))).collect();
    Reply::json(200, json!({"competitions": out}))
}

fn detail(principal: &Principal, id: &str) -> Reply {
    let (_, c) = tri!(public_competition(id));
    let now = clock::now();
    let mut v = public_view(&c, now);
    let entries = tri!(find(ENTRIES, "competition", id));
    let mine: Vec<Value> = entries
        .iter()
        .filter(|e| str_of(e, "owner") == principal.subject)
        .map(|e| json!({"entry": str_of(e, "id"), "photo": str_of(e, "photo"), "entered_at": u64_of(e, "entered_at")}))
        .collect();
    v.insert("my_entries".into(), json!(mine));
    v.insert("results_available".into(), json!(now >= u64_of(&c, "judging_closes_at")));
    Reply::json(200, Value::Object(v))
}

fn enter(principal: &Principal, id: &str, body: &str) -> Reply {
    tri!(require_active(principal));
    let Ok(Value::Object(req)) = serde_json::from_str::<Value>(body) else {
        return Reply::err(400, "bad_json");
    };
    let photo_id = req.get("photo_id").and_then(Value::as_str).unwrap_or_default().to_string();
    if photo_id.is_empty() {
        return Reply::err(400, "photo_id is required");
    }
    let c = match tri!(load(COMPETITIONS, id)) {
        Some((_, c)) if str_of(&c, "state") == "published" => c,
        _ => return Reply::err(404, "not_found"),
    };
    let now = clock::now();
    let (opens, closes) = (u64_of(&c, "opens_at"), u64_of(&c, "closes_at"));
    if now < opens || now >= closes {
        let detail = if now < opens { "not open yet" } else { "entries have closed" };
        return Reply::json(409, json!({"error": "competition_closed", "detail": detail}));
    }
    let Some((_, photo)) = tri!(load(PHOTOS, &photo_id)) else {
        return Reply::err(404, "not_found");
    };
    if str_of(&photo, "owner") != principal.subject {
        audit("competition.enter", "deny", &principal.subject, &photo_id);
        return Reply::err(403, "forbidden");
    }
    if str_of(&photo, "state") != "evaluated" {
        return Reply::err(409, "not_evaluated");
    }
    if is_hidden(&photo) {
        return Reply::err(409, "photo_hidden");
    }
    let sha = str_of(&photo, "sha256").to_string();
    let entries = tri!(find(ENTRIES, "competition", id));
    if let Some(e) = entries
        .iter()
        .find(|e| str_of(e, "photo") == photo_id || (!sha.is_empty() && str_of(e, "sha256") == sha))
    {
        let by = if str_of(e, "photo") == photo_id { "photo" } else { "sha256" };
        return Reply::json(
            409,
            json!({"error": "already_entered", "detail": format!("same {by}")}),
        );
    }
    let limit = c.get("max_entries_per_user").and_then(Value::as_u64).unwrap_or(1);
    let mine = entries.iter().filter(|e| str_of(e, "owner") == principal.subject).count() as u64;
    if mine >= limit {
        return Reply::json(409, json!({"error": "entry_limit", "limit": limit}));
    }
    let requirements = c.get("requirements").cloned().unwrap_or_else(|| json!({}));
    let verdict = crate::rules::verdict(&requirements, &photo, opens);
    if verdict.get("pass").and_then(Value::as_bool) != Some(true) {
        audit("competition.enter", "deny", &principal.subject, &photo_id);
        return Reply::json(422, json!({"error": "ineligible", "verdict": verdict}));
    }
    let (auto, flags) = crate::rules::auto_v1(&photo);
    let rec = json!({
        "competition": id, "photo": photo_id, "owner": principal.subject, "sha256": sha,
        "entered_at": now, "display_name": display_name(&principal.subject),
        "auto": auto, "auto_version": AUTO_VERSION, "auto_flags": flags, "verdict": verdict,
    });
    let idx: Vec<String> =
        ["competition", "photo", "owner", "sha256"].iter().map(|s| s.to_string()).collect();
    let entry = match records::create(ENTRIES, &rec.to_string(), &idx) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_error"),
    };
    audit("competition.enter", "allow", &principal.subject, &entry.id);
    Reply::json(201, Value::Object(doc(&entry)))
}

fn vote(principal: &Principal, id: &str, entry_id: &str, body: &str) -> Reply {
    tri!(require_active(principal));
    let Ok(Value::Object(req)) = serde_json::from_str::<Value>(body) else {
        return Reply::err(400, "bad_json");
    };
    let stars = match req.get("stars").and_then(Value::as_u64) {
        Some(s) if (1..=5).contains(&s) => s,
        _ => return Reply::err(400, "stars must be an integer 1..5"),
    };
    let c = match tri!(load(COMPETITIONS, id)) {
        Some((_, c)) if str_of(&c, "state") == "published" => c,
        _ => return Reply::err(404, "not_found"),
    };
    let (_, e) = tri!(entry_of(id, entry_id));
    if let Some((_, p)) = tri!(load(PHOTOS, str_of(&e, "photo"))) {
        if is_hidden(&p) {
            // Invisible on every shared surface: as far as a voter knows, it is gone.
            return Reply::err(404, "not_found");
        }
    }
    if str_of(&e, "owner") == principal.subject {
        return Reply::err(409, "own_entry");
    }
    let now = clock::now();
    if now < u64_of(&c, "opens_at") || now >= u64_of(&c, "voting_closes_at") {
        return Reply::err(409, "voting_closed");
    }
    let rec = json!({"competition": id, "entry": entry_id, "voter": principal.subject, "stars": stars, "at": now});
    tri!(upsert(VOTES, entry_id, "voter", &principal.subject, rec));
    audit("competition.vote", "allow", &principal.subject, entry_id);
    Reply::json(200, json!({"entry": entry_id, "stars": stars}))
}

// ---------------------------------------------------------------------------
// scoring

/// One entry's inputs, gathered from the store.
struct Scored {
    entry: Map<String, Value>,
    photo: Map<String, Value>,
    score: f64,
    row: Value,
}

fn mean(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        None
    } else {
        Some(xs.iter().sum::<f64>() / xs.len() as f64)
    }
}

/// The contract's parts, each 0..=1, and the 0..=100 score.
fn score_of(w: Weights, auto: f64, votes: &[f64], judges: &[f64]) -> (f64, f64, f64) {
    let v = mean(votes).map(|m| ((m - 1.0) / 4.0).clamp(0.0, 1.0)).unwrap_or(0.0);
    let j = mean(judges).map(|m| (m / 10.0).clamp(0.0, 1.0)).unwrap_or(0.0);
    let s = 100.0 * (w.auto * auto.clamp(0.0, 1.0) + w.votes * v + w.judges * j);
    (v, j, s)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// The latest record per `who_field`, grouped by entry: one vote per (voter, entry)
/// even if a race ever stored two.
fn latest_by(
    records_: Vec<Map<String, Value>>,
    who_field: &str,
    value_field: &str,
) -> HashMap<String, Vec<f64>> {
    let mut best: HashMap<(String, String), (u64, String, f64)> = HashMap::new();
    for r in records_ {
        let key = (str_of(&r, "entry").to_string(), str_of(&r, who_field).to_string());
        let cand = (u64_of(&r, "at"), str_of(&r, "id").to_string(), f64_of(&r, value_field));
        match best.get(&key) {
            Some(cur) if (cur.0, &cur.1) >= (cand.0, &cand.1) => {}
            _ => {
                best.insert(key, cand);
            }
        }
    }
    let mut out: HashMap<String, Vec<f64>> = HashMap::new();
    for ((entry, _), (_, _, v)) in best {
        out.entry(entry).or_default().push(v);
    }
    out
}

/// Every non-hidden entry, scored and ranked (highest first; ties → earlier entry).
fn ranking(id: &str, c: &Map<String, Value>, viewer: &str) -> Result<Vec<Scored>, Reply> {
    let w = weights_of(c);
    let entries = find(ENTRIES, "competition", id)?;
    let all_votes = find(VOTES, "competition", id)?;
    let my_votes: HashMap<String, u64> = all_votes
        .iter()
        .filter(|v| str_of(v, "voter") == viewer)
        .map(|v| (str_of(v, "entry").to_string(), u64_of(v, "stars")))
        .collect();
    let mut votes = latest_by(all_votes, "voter", "stars");
    let mut judges = latest_by(find(JUDGEMENTS, "competition", id)?, "curator", "score");
    let mut out = Vec::new();
    for e in entries {
        let Some((_, photo)) = load(PHOTOS, str_of(&e, "photo"))? else { continue };
        if is_hidden(&photo) {
            continue;
        }
        let eid = str_of(&e, "id").to_string();
        let vs = votes.remove(&eid).unwrap_or_default();
        let js = judges.remove(&eid).unwrap_or_default();
        let auto = f64_of(&e, "auto");
        let (v, j, s) = score_of(w, auto, &vs, &js);
        let flags = e.get("auto_flags").cloned().unwrap_or_else(|| json!([]));
        let row = json!({
            "entry": eid,
            "photo": str_of(&e, "photo"),
            "entrant": {"display_name": str_of(&e, "display_name")},
            "mine": str_of(&e, "owner") == viewer,
            "my_vote": my_votes.get(&eid),
            "entered_at": u64_of(&e, "entered_at"),
            "score": round2(s),
            "parts": {
                "auto":   {"value": auto, "version": e.get("auto_version").cloned().unwrap_or(json!(AUTO_VERSION)),
                           "flags": flags, "weight": w.auto},
                "votes":  {"value": v, "count": vs.len(), "mean": mean(&vs), "weight": w.votes,
                           "no_inputs": vs.is_empty()},
                "judges": {"value": j, "count": js.len(), "mean": mean(&js), "weight": w.judges,
                           "no_inputs": js.is_empty()},
            },
        });
        out.push(Scored { entry: e, photo, score: s, row });
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| u64_of(&a.entry, "entered_at").cmp(&u64_of(&b.entry, "entered_at")))
            .then_with(|| str_of(&a.entry, "id").cmp(str_of(&b.entry, "id")))
    });
    Ok(out)
}

fn leaderboard(principal: &Principal, id: &str) -> Reply {
    let (_, c) = tri!(public_competition(id));
    let now = clock::now();
    let ranked = tri!(ranking(id, &c, &principal.subject));
    let rows: Vec<Value> = ranked
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let mut row = s.row;
            row["rank"] = json!(i + 1);
            row["thumb_url"] = thumb_url(&s.photo);
            row
        })
        .collect();
    Reply::json(
        200,
        json!({"competition": id, "phase": phase(&c, now), "weights": c.get("weights"), "entries": rows}),
    )
}

// ---------------------------------------------------------------------------
// results

/// Freeze the ranking into the competition record once, then credit every
/// unclaimed prize. Returns the stored `results`.
fn freeze_and_credit(id: &str, viewer: &str) -> Result<Map<String, Value>, Reply> {
    // 1. freeze
    let results = loop_freeze(id, viewer)?;
    // 2. claim unclaimed prizes, credit them, record the outcome
    let pending: Vec<usize> = prizes_of(&results)
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            matches!(p.get("status").and_then(Value::as_str), Some("pending" | "failed"))
        })
        .map(|(i, _)| i)
        .collect();
    if pending.is_empty() {
        return Ok(results);
    }
    let claimed = match claim(id, &pending)? {
        Some(v) => v,
        // Somebody else claimed them first; they credit, we serve.
        None => return current_results(id),
    };
    let comp = load(COMPETITIONS, id)?.map(|(_, c)| c).unwrap_or_default();
    let journey = comp.get("journey").and_then(Value::as_str).map(str::to_string);
    let mut outcome: Vec<(usize, &'static str, Value)> = Vec::new();
    for i in claimed {
        let p = &prizes_of(&results)[i];
        let place = p.get("place").and_then(Value::as_u64).unwrap_or(0);
        let xp = p.get("xp").and_then(Value::as_u64).unwrap_or(0);
        let source_id = format!("{id}#{place}");
        let r = credit(
            p.get("owner").and_then(Value::as_str).unwrap_or_default(),
            journey.as_deref(),
            "competition",
            &source_id,
            p.get("photo").and_then(Value::as_str).unwrap_or_default(),
            p.get("sha256").and_then(Value::as_str).unwrap_or_default(),
            xp,
        );
        match r {
            Ok(_) => {
                audit(
                    "competition.prize",
                    "allow",
                    p.get("owner").and_then(Value::as_str).unwrap_or_default(),
                    &source_id,
                );
                outcome.push((i, "credited", Value::Null));
            }
            Err(reply) => outcome.push((i, "failed", reply.json.clone())),
        }
    }
    record_outcome(id, &outcome)
}

fn prizes_of(results: &Map<String, Value>) -> Vec<Map<String, Value>> {
    results
        .get("prizes")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|p| p.as_object().cloned()).collect())
        .unwrap_or_default()
}

fn current_results(id: &str) -> Result<Map<String, Value>, Reply> {
    match load(COMPETITIONS, id)? {
        Some((_, c)) => {
            Ok(c.get("results").and_then(Value::as_object).cloned().unwrap_or_default())
        }
        None => Err(Reply::err(404, "not_found")),
    }
}

fn loop_freeze(id: &str, viewer: &str) -> Result<Map<String, Value>, Reply> {
    for _ in 0..5 {
        let Some((entry, mut c)) = load(COMPETITIONS, id)? else {
            return Err(Reply::err(404, "not_found"));
        };
        if let Some(Value::Object(r)) = c.get("results") {
            return Ok(r.clone());
        }
        let ranked = ranking(id, &c, viewer)?;
        let prizes_xp: Vec<u64> = c
            .get("prizes_xp")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        let mut rows = Vec::new();
        let mut prizes = Vec::new();
        for (i, s) in ranked.iter().enumerate() {
            let place = i + 1;
            let mut row = s.row.clone();
            row["rank"] = json!(place);
            // Per-viewer fields do not belong in a frozen, shared record.
            if let Some(o) = row.as_object_mut() {
                o.remove("mine");
                o.remove("my_vote");
            }
            row["owner"] = json!(str_of(&s.entry, "owner"));
            rows.push(row);
            if let Some(&xp) = prizes_xp.get(i) {
                prizes.push(json!({
                    "place": place, "entry": str_of(&s.entry, "id"), "photo": str_of(&s.entry, "photo"),
                    "owner": str_of(&s.entry, "owner"), "sha256": str_of(&s.entry, "sha256"),
                    "display_name": str_of(&s.entry, "display_name"), "xp": xp,
                    // A 0-XP place has nothing to credit.
                    "status": if xp == 0 { "credited" } else { "pending" },
                }));
            }
        }
        let results = json!({
            "frozen_at": clock::now(), "auto_version": AUTO_VERSION,
            "ranking": rows, "prizes": prizes,
        });
        c.insert("results".into(), results.clone());
        match records::update(COMPETITIONS, id, &data_of(&c), entry.revision) {
            Ok(_) => {
                audit("competition.results", "allow", viewer, id);
                return Ok(results.as_object().cloned().unwrap_or_default());
            }
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(_) => return Err(Reply::err(500, "store_error")),
        }
    }
    Err(Reply::err(503, "busy"))
}

/// Mark `wanted` prizes `claimed` if they are still pending/failed. `Some(claimed
/// indexes)` when this request now owns them, `None` when none were left.
fn claim(id: &str, wanted: &[usize]) -> Result<Option<Vec<usize>>, Reply> {
    for _ in 0..5 {
        let Some((entry, mut c)) = load(COMPETITIONS, id)? else {
            return Err(Reply::err(404, "not_found"));
        };
        let mut got = Vec::new();
        if let Some(prizes) =
            c.get_mut("results").and_then(|r| r.get_mut("prizes")).and_then(Value::as_array_mut)
        {
            for &i in wanted {
                if let Some(p) = prizes.get_mut(i) {
                    if matches!(p["status"].as_str(), Some("pending" | "failed")) {
                        p["status"] = json!("claimed");
                        p["claimed_at"] = json!(clock::now());
                        got.push(i);
                    }
                }
            }
        }
        if got.is_empty() {
            return Ok(None);
        }
        match records::update(COMPETITIONS, id, &data_of(&c), entry.revision) {
            Ok(_) => return Ok(Some(got)),
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(_) => return Err(Reply::err(500, "store_error")),
        }
    }
    Err(Reply::err(503, "busy"))
}

fn record_outcome(
    id: &str,
    outcome: &[(usize, &'static str, Value)],
) -> Result<Map<String, Value>, Reply> {
    for _ in 0..5 {
        let Some((entry, mut c)) = load(COMPETITIONS, id)? else {
            return Err(Reply::err(404, "not_found"));
        };
        if let Some(prizes) =
            c.get_mut("results").and_then(|r| r.get_mut("prizes")).and_then(Value::as_array_mut)
        {
            for (i, status, err) in outcome {
                if let Some(p) = prizes.get_mut(*i) {
                    p["status"] = json!(status);
                    if *status == "credited" {
                        p["credited_at"] = json!(clock::now());
                        if let Some(o) = p.as_object_mut() {
                            o.remove("error");
                        }
                    } else {
                        p["error"] = err.clone();
                    }
                }
            }
        }
        match records::update(COMPETITIONS, id, &data_of(&c), entry.revision) {
            Ok(e) => {
                return Ok(doc(&e)
                    .get("results")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default());
            }
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(_) => return Err(Reply::err(500, "store_error")),
        }
    }
    Err(Reply::err(503, "busy"))
}

fn results(principal: &Principal, id: &str) -> Reply {
    let (_, c) = tri!(public_competition(id));
    let at = u64_of(&c, "judging_closes_at");
    if clock::now() < at {
        return Reply::json(409, json!({"error": "results_pending", "available_at": at}));
    }
    let r = tri!(freeze_and_credit(id, &principal.subject));
    // The frozen ranking is history; a photo hidden since is still invisible on
    // this shared surface, but places are not renumbered and prizes stand.
    let visible = |row: &Value| -> bool {
        let pid = row.get("photo").and_then(Value::as_str).unwrap_or_default();
        !matches!(load(PHOTOS, pid), Ok(Some((_, p))) if is_hidden(&p))
    };
    let public_row = |row: &Value| -> Value {
        let mut row = row.clone();
        let owner = row.get("owner").and_then(Value::as_str).unwrap_or_default().to_string();
        if let Some(o) = row.as_object_mut() {
            o.remove("owner");
            o.insert("mine".into(), json!(owner == principal.subject));
        }
        row
    };
    let ranking: Vec<Value> = r
        .get("ranking")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|x| visible(x)).map(public_row).collect())
        .unwrap_or_default();
    let winners: Vec<Value> = prizes_of(&r)
        .into_iter()
        .map(Value::Object)
        .filter(visible)
        .map(|p| {
            let owner = p["owner"].as_str().unwrap_or_default().to_string();
            json!({
                "place": p["place"], "entry": p["entry"], "photo": p["photo"],
                "entrant": {"display_name": p["display_name"]}, "xp": p["xp"],
                "credited": p["status"] == "credited", "mine": owner == principal.subject,
            })
        })
        .collect();
    Reply::json(
        200,
        json!({
            "competition": id, "frozen_at": r.get("frozen_at"), "auto_version": r.get("auto_version"),
            "ranking": ranking, "winners": winners,
        }),
    )
}

// ---------------------------------------------------------------------------
// Prize XP goes through `progress::credit` (the ledger, the idempotency on
// (user, source, source_id) and level-ups live there).
fn credit(
    user: &str,
    journey: Option<&str>,
    source: &str,
    source_id: &str,
    photo_id: &str,
    sha256: &str,
    xp: u64,
) -> Result<(), Reply> {
    crate::progress::credit(user, journey, source, source_id, photo_id, sha256, xp).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_must_sum_to_one() {
        assert!(parse_weights(&json!({"auto": 0.4, "votes": 0.3, "judges": 0.3})).is_ok());
        assert!(parse_weights(&json!({"auto": 1})).is_ok());
        assert!(parse_weights(&json!({"auto": 0.5, "votes": 0.3, "judges": 0.3})).is_err());
        assert!(parse_weights(&json!({"auto": -0.1, "votes": 0.8, "judges": 0.3})).is_err());
        assert!(parse_weights(&json!({"auto": 0.5, "luck": 0.5})).is_err());
        assert!(parse_weights(&json!([1])).is_err());
    }

    #[test]
    fn windows_are_ordered() {
        assert!(check_windows(1, 2, 2, 2).is_ok());
        assert!(check_windows(2, 2, 3, 4).is_err());
        assert!(check_windows(1, 3, 2, 4).is_err());
        assert!(check_windows(1, 2, 4, 3).is_err());
    }

    #[test]
    fn score_mixes_normalised_parts() {
        let w = Weights { auto: 0.4, votes: 0.3, judges: 0.3 };
        let (v, j, s) = score_of(w, 0.55, &[5.0, 5.0], &[10.0]);
        assert_eq!((v, j), (1.0, 1.0));
        assert!((s - 82.0).abs() < 1e-9);
        let (v, j, s) = score_of(w, 0.5, &[], &[]);
        assert_eq!((v, j), (0.0, 0.0));
        assert!((s - 20.0).abs() < 1e-9);
    }

    #[test]
    fn one_vote_per_voter_latest_wins() {
        let r = |entry: &str, voter: &str, at: u64, stars: u64| {
            let mut m = Map::new();
            m.insert("entry".into(), json!(entry));
            m.insert("voter".into(), json!(voter));
            m.insert("at".into(), json!(at));
            m.insert("stars".into(), json!(stars));
            m.insert("id".into(), json!(format!("{entry}{voter}{at}")));
            m
        };
        let got = latest_by(
            vec![r("e", "a", 1, 2), r("e", "a", 5, 4), r("e", "b", 1, 1)],
            "voter",
            "stars",
        );
        let mut v = got["e"].clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(v, vec![1.0, 4.0]);
    }

    #[test]
    fn display_name_hides_the_subject() {
        assert_eq!(name_from("usr_0123456789abcdef", ""), "photographer-abcdef");
        assert_eq!(name_from("usr_x", "ada@example.test"), "ada");
    }
}
