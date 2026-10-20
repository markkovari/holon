//! A journey's chain and badge are over its *published* quests in `journey.quests` order.
//! Races: records:store has no unique constraint, so the writer whose ULID is not the smallest deletes its row.

use crate::bindings::p3::http::types::Method;
use crate::bindings::records::store::store as records;
use crate::clock;
use crate::moderation::{is_hidden, require_active};
use crate::{audit, introspect, Reply, Route};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

pub(crate) const JOURNEYS: &str = "journeys";
pub(crate) const QUESTS: &str = "quests";
const PHOTOS: &str = "photos";
const SUBMISSIONS: &str = "submissions";
const LEDGER: &str = "xp_ledger";
const BADGES: &str = "badges";
/// Must match competitions.rs's interim ledger rows.
const LEDGER_INDEX: &[&str] = &["user", "sha256", "source_id"];

/// Record ids are ULIDs; anything else is a 404 before it reaches the store.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub(crate) fn doc(entry: &records::Entry) -> Map<String, Value> {
    let mut m = match serde_json::from_str::<Value>(&entry.data) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    m.insert("id".into(), json!(entry.id));
    m
}

pub(crate) fn data_of(m: &Map<String, Value>) -> String {
    let mut d = m.clone();
    d.remove("id");
    Value::Object(d).to_string()
}

pub(crate) fn str_of<'a>(m: &'a Map<String, Value>, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub(crate) fn u64_of(m: &Map<String, Value>, key: &str) -> u64 {
    m.get(key).and_then(Value::as_u64).unwrap_or(0)
}

pub(crate) fn store_err() -> Reply {
    Reply::err(500, "store_error")
}

type EntryDoc = (records::Entry, Map<String, Value>);

pub(crate) fn load(collection: &str, id: &str) -> Result<Option<EntryDoc>, Reply> {
    if !valid_id(id) {
        return Ok(None);
    }
    match records::get(collection, id) {
        Ok(e) => {
            let m = doc(&e);
            Ok(Some((e, m)))
        }
        Err(records::StoreError::NotFound) => Ok(None),
        Err(_) => Err(store_err()),
    }
}

pub(crate) fn find(
    collection: &str,
    field: &str,
    value: &str,
) -> Result<Vec<Map<String, Value>>, Reply> {
    let v = serde_json::to_string(value).unwrap_or_default();
    match records::find_by(collection, field, &v) {
        Ok(es) => Ok(es.iter().map(doc).collect()),
        Err(records::StoreError::NotFound) => Ok(Vec::new()),
        Err(_) => Err(store_err()),
    }
}

pub(crate) fn list_all(collection: &str) -> Result<Vec<Map<String, Value>>, Reply> {
    let mut out = Vec::new();
    let mut after = String::new();
    loop {
        let page = match records::list_records(collection, 200, &after) {
            Ok(p) => p,
            Err(records::StoreError::NotFound) => break,
            Err(_) => return Err(store_err()),
        };
        out.extend(page.entries.iter().map(doc));
        if page.next.is_empty() || page.entries.is_empty() {
            break;
        }
        after = page.next;
    }
    Ok(out)
}

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(r) => return r,
        }
    };
}
pub(crate) use tri;

pub(crate) fn level_of(levels: &Value, xp: u64) -> (u64, Option<u64>) {
    let mut level = 1;
    let mut next = None;
    for l in levels.as_array().into_iter().flatten() {
        let (n, need) = (
            l.get("level").and_then(Value::as_u64).unwrap_or(0),
            l.get("xp").and_then(Value::as_u64).unwrap_or(0),
        );
        if xp >= need {
            level = level.max(n);
        } else if next.is_none() {
            next = Some(need);
        }
    }
    (level, next)
}

pub struct Credit {
    pub awarded: u64,
    /// "already_rewarded" (photo or sha256 already paid) or "already_passed" (quest paid with another photo).
    pub reason: Option<String>,
    pub level_up: Option<Value>,
}

impl Credit {
    fn none(reason: Option<&str>) -> Self {
        Credit { awarded: 0, reason: reason.map(str::to_string), level_up: None }
    }
}

fn is_row(r: &Map<String, Value>, user: &str, source: &str, source_id: &str) -> bool {
    str_of(r, "user") == user
        && str_of(r, "source") == source
        && str_of(r, "source_id") == source_id
}

/// The smallest id wins a race.
fn first_id(rows: &[Map<String, Value>]) -> Option<String> {
    rows.iter().map(|r| str_of(r, "id").to_string()).min()
}

/// Idempotent per (user, source, source_id, photo). For quests a file (by sha256) earns XP at most once
/// across every quest and user; competition prizes neither consume nor are blocked by that.
pub fn credit(
    user: &str,
    journey: Option<&str>,
    source: &str,
    source_id: &str,
    photo_id: &str,
    sha256: &str,
    xp: u64,
) -> Result<Credit, Reply> {
    let mine = find(LEDGER, "user", user)?;
    let paid_for_source: Vec<&Map<String, Value>> =
        mine.iter().filter(|r| is_row(r, user, source, source_id)).collect();
    if paid_for_source.iter().any(|r| str_of(r, "photo") == photo_id) {
        return Ok(Credit::none(Some("already_rewarded")));
    }
    let sha_counts = source == "quest" && !sha256.is_empty();
    if sha_counts && find(LEDGER, "sha256", sha256)?.iter().any(|r| str_of(r, "source") == "quest")
    {
        return Ok(Credit::none(Some("already_rewarded")));
    }
    if !paid_for_source.is_empty() {
        return Ok(Credit::none(Some(if source == "quest" {
            "already_passed"
        } else {
            "already_rewarded"
        })));
    }
    if xp == 0 {
        return Ok(Credit::none(None));
    }

    let before = journey.map(|j| journey_xp(&mine, j)).unwrap_or(0);
    let row = json!({
        "user": user, "journey": journey, "source": source, "source_id": source_id,
        "photo": photo_id, "sha256": sha256, "xp": xp, "at": clock::now(),
    });
    let idx: Vec<String> = LEDGER_INDEX.iter().map(|s| s.to_string()).collect();
    let ours = records::create(LEDGER, &row.to_string(), &idx).map_err(|_| store_err())?.id;

    // First writer wins (module docs, "Races").
    let rivals: Vec<Map<String, Value>> = find(LEDGER, "source_id", source_id)?
        .into_iter()
        .filter(|r| is_row(r, user, source, source_id))
        .collect();
    let mut lost = first_id(&rivals).is_some_and(|w| w != ours);
    if !lost && sha_counts {
        let same: Vec<Map<String, Value>> = find(LEDGER, "sha256", sha256)?
            .into_iter()
            .filter(|r| str_of(r, "source") == "quest")
            .collect();
        lost = first_id(&same).is_some_and(|w| w != ours);
    }
    if lost {
        let _ = records::delete(LEDGER, &ours);
        return Ok(Credit::none(Some("already_rewarded")));
    }
    audit("xp.credit", "allow", user, &format!("{source}:{source_id} +{xp}"));

    let level_up = match journey {
        Some(j) => {
            let levels =
                load(JOURNEYS, j)?.map(|(_, m)| m.get("levels").cloned().unwrap_or(Value::Null));
            let levels = levels.unwrap_or(Value::Null);
            let (from, _) = level_of(&levels, before);
            let (to, _) = level_of(&levels, before + xp);
            (to > from).then(|| json!({"journey": j, "from": from, "to": to}))
        }
        None => None,
    };
    Ok(Credit { awarded: xp, reason: None, level_up })
}

fn journey_xp(ledger: &[Map<String, Value>], journey: &str) -> u64 {
    ledger.iter().filter(|r| str_of(r, "journey") == journey).map(|r| u64_of(r, "xp")).sum()
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Get, ["api", "journeys"]) => journeys(route),
        (Method::Get, ["api", "journeys", id]) => journey(route, id),
        (Method::Get, ["api", "quests", id]) => quest(route, id),
        (Method::Post, ["api", "quests", id, "submissions"]) => submit(route, id, body),
        (Method::Get, ["api", "quests", id, "submissions"]) => my_submissions(route, id),
        (Method::Get, ["api", "me", "progress"]) => me_progress(route),
        _ => Reply::err(404, "not_found"),
    }
}

struct Mine {
    ledger: Vec<Map<String, Value>>,
    passed: HashSet<String>,
    badges: Vec<Map<String, Value>>,
}

fn mine(user: &str) -> Result<Mine, Reply> {
    let passed = find(SUBMISSIONS, "user", user)?
        .iter()
        .filter(|s| s.get("pass").and_then(Value::as_bool) == Some(true))
        .map(|s| str_of(s, "quest").to_string())
        .collect();
    Ok(Mine { ledger: find(LEDGER, "user", user)?, passed, badges: find(BADGES, "user", user)? })
}

fn chain(journey: &Map<String, Value>) -> Result<Vec<Map<String, Value>>, Reply> {
    let mut out = Vec::new();
    for id in journey.get("quests").and_then(Value::as_array).into_iter().flatten() {
        let Some(id) = id.as_str() else { continue };
        if let Some((_, q)) = load(QUESTS, id)? {
            if str_of(&q, "state") == "published" && str_of(&q, "journey") == str_of(journey, "id")
            {
                out.push(q);
            }
        }
    }
    Ok(out)
}

/// Quest n+1 is locked until quest n is passed.
fn states(chain: &[Map<String, Value>], passed: &HashSet<String>) -> Vec<&'static str> {
    let mut prev_passed = true;
    chain
        .iter()
        .map(|q| {
            let done = passed.contains(str_of(q, "id"));
            let s = if done {
                "passed"
            } else if prev_passed {
                "open"
            } else {
                "locked"
            };
            prev_passed = done;
            s
        })
        .collect()
}

/// Half-open window `[starts_at, ends_at)`.
fn window(q: &Map<String, Value>, now: u64) -> &'static str {
    if now < u64_of(q, "starts_at") {
        "upcoming"
    } else if q.get("ends_at").and_then(Value::as_u64).is_some_and(|e| now >= e) {
        "ended"
    } else {
        "running"
    }
}

fn quest_view(q: &Map<String, Value>, state: &str, now: u64) -> Value {
    json!({
        "id": q.get("id"), "journey": q.get("journey"), "title": q.get("title"),
        "description": q.get("description"), "xp": q.get("xp"),
        "starts_at": q.get("starts_at"), "ends_at": q.get("ends_at"),
        "requirements": q.get("requirements"), "state": state, "window": window(q, now),
    })
}

fn standing(j: &Map<String, Value>, m: &Mine) -> Value {
    let id = str_of(j, "id");
    let xp = journey_xp(&m.ledger, id);
    let levels = j.get("levels").cloned().unwrap_or(Value::Null);
    let (level, next) = level_of(&levels, xp);
    let badge = m.badges.iter().find(|b| str_of(b, "journey") == id);
    json!({
        "xp": xp, "level": level, "next_level_xp": next,
        "badge": badge.map(|b| json!({"name": b.get("name"), "at": b.get("at")})),
    })
}

fn journey_head(j: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for k in ["id", "title", "description", "levels", "badge"] {
        out.insert(k.into(), j.get(k).cloned().unwrap_or(Value::Null));
    }
    out
}

fn published_journey(id: &str) -> Result<Option<Map<String, Value>>, Reply> {
    Ok(load(JOURNEYS, id)?.map(|(_, j)| j).filter(|j| str_of(j, "state") == "published"))
}

fn journeys(route: &Route) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let m = tri!(mine(&principal.subject));
    let all = tri!(list_all(JOURNEYS));
    let mut out = Vec::new();
    for j in all.iter().filter(|j| str_of(j, "state") == "published") {
        let c = tri!(chain(j));
        let mut v = journey_head(j);
        v.insert("progress".into(), standing(j, &m));
        v.insert("quest_count".into(), json!(c.len()));
        v.insert(
            "passed_count".into(),
            json!(c.iter().filter(|q| m.passed.contains(str_of(q, "id"))).count()),
        );
        out.push(Value::Object(v));
    }
    Reply::json(200, json!({ "journeys": out }))
}

fn journey(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let Some(j) = tri!(published_journey(id)) else { return Reply::err(404, "not_found") };
    let m = tri!(mine(&principal.subject));
    let c = tri!(chain(&j));
    let now = clock::now();
    let quests: Vec<Value> =
        c.iter().zip(states(&c, &m.passed)).map(|(q, s)| quest_view(q, s, now)).collect();
    let mut v = journey_head(&j);
    v.insert("progress".into(), standing(&j, &m));
    v.insert("quests".into(), json!(quests));
    Reply::json(200, Value::Object(v))
}

type QuestJourney = (Map<String, Value>, Map<String, Value>);

fn visible_quest(id: &str) -> Result<Option<QuestJourney>, Reply> {
    let Some((_, q)) = load(QUESTS, id)? else { return Ok(None) };
    if str_of(&q, "state") == "draft" {
        return Ok(None);
    }
    let Some(j) = published_journey(str_of(&q, "journey"))? else { return Ok(None) };
    Ok(Some((q, j)))
}

fn state_in(
    q: &Map<String, Value>,
    j: &Map<String, Value>,
    passed: &HashSet<String>,
) -> Result<&'static str, Reply> {
    let c = chain(j)?;
    let st = states(&c, passed);
    Ok(c.iter()
        .position(|x| str_of(x, "id") == str_of(q, "id"))
        .map(|i| st[i])
        .unwrap_or(if passed.contains(str_of(q, "id")) { "passed" } else { "archived" }))
}

fn quest(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let Some((q, j)) = tri!(visible_quest(id)) else { return Reply::err(404, "not_found") };
    if str_of(&q, "state") != "published" {
        return Reply::err(404, "not_found");
    }
    let m = tri!(mine(&principal.subject));
    let state = tri!(state_in(&q, &j, &m.passed));
    let mut v = quest_view(&q, state, clock::now());
    v["journey_title"] = j.get("title").cloned().unwrap_or(Value::Null);
    Reply::json(200, v)
}

#[derive(serde::Deserialize)]
struct SubmitReq {
    #[serde(default)]
    photo_id: String,
}

/// Refusals in CONTRACT.md's order, all before judging.
fn submit(route: &Route, id: &str, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let req = guestauth::guest_parse_body!(body, SubmitReq);
    let photo_id = req.photo_id.trim().to_string();
    if photo_id.is_empty() {
        return Reply::err(400, "photo_id is required");
    }
    let Some((q, j)) = tri!(visible_quest(id)) else { return Reply::err(404, "not_found") };
    let Some((_, photo)) = tri!(load(PHOTOS, &photo_id)) else {
        return Reply::err(404, "not_found");
    };
    guestauth::guest_deny_unless!(
        str_of(&photo, "owner") == principal.subject,
        principal,
        "quest.submit",
        &photo_id
    );
    if str_of(&photo, "state") != "evaluated" {
        return Reply::err(409, "not_evaluated");
    }
    if is_hidden(&photo) {
        return Reply::err(409, "photo_hidden");
    }
    let m = tri!(mine(&principal.subject));
    if tri!(state_in(&q, &j, &m.passed)) == "locked" {
        return Reply::err(409, "quest_locked");
    }
    let now = clock::now();
    match window(&q, now) {
        "upcoming" => return Reply::err(409, "quest_not_started"),
        "ended" => return Reply::err(409, "quest_ended"),
        _ => {}
    }
    if str_of(&q, "state") != "published" {
        return Reply::err(409, "quest_not_published");
    }
    if let Err(r) = require_active(&principal) {
        return r;
    }

    let journey_id = str_of(&j, "id").to_string();
    let requirements = q.get("requirements").cloned().unwrap_or(Value::Null);
    let verdict = crate::rules::verdict(&requirements, &photo, u64_of(&q, "starts_at"));
    let pass = verdict["pass"] == json!(true);
    let sha256 = str_of(&photo, "sha256").to_string();

    let (mut awarded, mut reason, mut level_up, mut badge) = (0, None, None, Value::Null);
    if pass {
        let c = tri!(credit(
            &principal.subject,
            Some(&journey_id),
            "quest",
            id,
            &photo_id,
            &sha256,
            u64_of(&q, "xp"),
        ));
        (awarded, reason, level_up) = (c.awarded, c.reason, c.level_up);
        let mut passed = m.passed.clone();
        passed.insert(id.to_string());
        badge = tri!(grant_badge(&principal.subject, &j, &passed, &m.badges));
    }

    let rec = json!({
        "user": principal.subject, "quest": id, "journey": journey_id, "photo": photo_id,
        "sha256": sha256, "verdict": verdict, "pass": pass, "xp_awarded": awarded,
        "xp_reason": reason, "level_up": level_up, "badge": badge, "at": now,
    });
    let entry = match records::create(SUBMISSIONS, &rec.to_string(), &["user".to_string()]) {
        Ok(e) => e,
        Err(_) => return store_err(),
    };
    audit("quest.submit", "allow", &principal.subject, &format!("{id} {photo_id} pass={pass}"));
    Reply::json(201, Value::Object(doc(&entry)))
}

fn grant_badge(
    user: &str,
    j: &Map<String, Value>,
    passed: &HashSet<String>,
    have: &[Map<String, Value>],
) -> Result<Value, Reply> {
    let jid = str_of(j, "id");
    let Some(name) = j.get("badge").and_then(|b| b.get("name")).and_then(Value::as_str) else {
        return Ok(Value::Null);
    };
    if have.iter().any(|b| str_of(b, "journey") == jid) {
        return Ok(Value::Null);
    }
    let c = chain(j)?;
    if c.is_empty() || !c.iter().all(|q| passed.contains(str_of(q, "id"))) {
        return Ok(Value::Null);
    }
    let row = json!({"user": user, "journey": jid, "name": name, "at": clock::now()});
    let ours = records::create(BADGES, &row.to_string(), &["user".to_string()])
        .map_err(|_| store_err())?
        .id;
    let rivals: Vec<Map<String, Value>> =
        find(BADGES, "user", user)?.into_iter().filter(|b| str_of(b, "journey") == jid).collect();
    if first_id(&rivals).is_some_and(|w| w != ours) {
        let _ = records::delete(BADGES, &ours);
        return Ok(Value::Null);
    }
    audit("badge.grant", "allow", user, jid);
    Ok(json!({"journey": jid, "name": name}))
}

fn newest_first(v: &mut [Map<String, Value>]) {
    v.sort_by(|a, b| {
        u64_of(b, "at").cmp(&u64_of(a, "at")).then_with(|| str_of(b, "id").cmp(str_of(a, "id")))
    });
}

fn my_submissions(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    if tri!(visible_quest(id)).is_none() {
        return Reply::err(404, "not_found");
    }
    let mut subs: Vec<Map<String, Value>> = tri!(find(SUBMISSIONS, "user", &principal.subject))
        .into_iter()
        .filter(|s| str_of(s, "quest") == id)
        .collect();
    newest_first(&mut subs);
    Reply::json(200, json!({ "submissions": subs }))
}

fn me_progress(route: &Route) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let m = tri!(mine(&principal.subject));
    let all = tri!(list_all(JOURNEYS));
    let mut journeys = Vec::new();
    for j in &all {
        let id = str_of(j, "id");
        let involved = m.ledger.iter().any(|r| str_of(r, "journey") == id)
            || m.badges.iter().any(|b| str_of(b, "journey") == id);
        if str_of(j, "state") != "published" && !involved {
            continue;
        }
        let mut v = standing(j, &m);
        v["journey"] = json!(id);
        v["title"] = j.get("title").cloned().unwrap_or(Value::Null);
        v["state"] = j.get("state").cloned().unwrap_or(Value::Null);
        journeys.push(v);
    }
    let total: u64 = m.ledger.iter().map(|r| u64_of(r, "xp")).sum();
    let mut ledger = m.ledger.clone();
    newest_first(&mut ledger);
    let badges: Vec<Value> = m
        .badges
        .iter()
        .map(|b| json!({"journey": b.get("journey"), "name": b.get("name"), "at": b.get("at")}))
        .collect();
    Reply::json(
        200,
        json!({ "total_xp": total, "journeys": journeys, "badges": badges, "ledger": ledger }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(id: &str) -> Map<String, Value> {
        json!({"id": id}).as_object().cloned().unwrap()
    }

    #[test]
    fn levels_are_the_highest_threshold_reached() {
        let levels =
            json!([{"level": 1, "xp": 0}, {"level": 2, "xp": 100}, {"level": 3, "xp": 250}]);
        assert_eq!(level_of(&levels, 0), (1, Some(100)));
        assert_eq!(level_of(&levels, 99), (1, Some(100)));
        assert_eq!(level_of(&levels, 100), (2, Some(250)));
        assert_eq!(level_of(&levels, 270), (3, None));
        assert_eq!(level_of(&Value::Null, 50), (1, None));
    }

    #[test]
    fn quest_n_plus_one_is_locked_until_n_is_passed() {
        let chain = vec![q("a"), q("b"), q("c")];
        let none = HashSet::new();
        assert_eq!(states(&chain, &none), ["open", "locked", "locked"]);
        let a: HashSet<String> = ["a".to_string()].into();
        assert_eq!(states(&chain, &a), ["passed", "open", "locked"]);
        let ab: HashSet<String> = ["a".to_string(), "b".to_string()].into();
        assert_eq!(states(&chain, &ab), ["passed", "passed", "open"]);
        // Reordered after b was passed: b stays passed and unlocks the next.
        let b: HashSet<String> = ["b".to_string()].into();
        assert_eq!(states(&chain, &b), ["open", "passed", "open"]);
    }

    #[test]
    fn the_window_is_half_open() {
        let m = json!({"starts_at": 100, "ends_at": 200}).as_object().cloned().unwrap();
        assert_eq!(window(&m, 99), "upcoming");
        assert_eq!(window(&m, 100), "running");
        assert_eq!(window(&m, 199), "running");
        assert_eq!(window(&m, 200), "ended");
        let open = json!({"starts_at": 100, "ends_at": null}).as_object().cloned().unwrap();
        assert_eq!(window(&open, u64::MAX), "running");
    }

    #[test]
    fn ids() {
        assert!(valid_id("01K5Y2Z7Q9ABCDEF0123456789"));
        assert!(!valid_id("../x"));
        assert!(!valid_id(""));
    }
}
