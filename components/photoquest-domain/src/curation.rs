//! A curator's side of the game (CONTRACT.md "Journeys, quests, levels", "Game
//! routes" → `curation.rs`): create, edit, publish and archive journeys and
//! quests, and list them in every state.
//!
//! | route | answer |
//! |---|---|
//! | `GET  /api/curator/journeys` | `?state=draft\|published\|archived\|all&q=&limit=&after=` → 200 `{journeys, next}`, newest first |
//! | `POST /api/curator/journeys` | `{title, description?, levels?, badge?}` → 201 the journey (`draft`, `quests: []`) |
//! | `GET  /api/curator/journeys/{id}` | 200 the journey, plus `quest_docs` (its quests, in order, every state) |
//! | `PUT  /api/curator/journeys/{id}` | any of `{title, description, levels, badge, quests}` → 200 |
//! | `POST /api/curator/journeys/{id}/publish` / `archive` | 200 |
//! | `GET  /api/curator/quests` | the same filters → 200 `{quests, next}`, newest first |
//! | `POST /api/curator/quests` | `{journey, title, description?, xp?, starts_at?, ends_at?, requirements?}` → 201 (`draft`) |
//! | `GET  /api/curator/quests/{id}` | 200 |
//! | `PUT  /api/curator/quests/{id}` | any of `{title, description, xp, starts_at, ends_at, requirements}` → 200 |
//! | `POST /api/curator/quests/{id}/publish` / `archive` | 200 |
//!
//! Rules this module adds to the contract:
//!
//! * **Who may edit.** Every route needs the `curator` role (`403 forbidden_role`);
//!   writes also need an active account (`403 suspended`). A journey — and every
//!   quest in it — is edited by the curator who created it; another curator gets
//!   `403 forbidden`, unless they are also an `admin` (a way out when a curator
//!   leaves). Reads are open to every curator.
//! * **Levels** default to `[{level: 1, xp: 0}]`. They must be consecutive levels
//!   from 1 with strictly ascending XP, the first `{1, 0}` — else `400 bad_levels`.
//! * **Quest order.** Creating a quest appends it to its journey's `quests`; `PUT
//!   …/journeys/{id} {quests}` reorders (it must be the same set of ids).
//!   Publishing a journey keeps that order and appends any quest of the journey the
//!   list is missing. A quest never moves to another journey.
//! * **Publishing a quest whose journey is a draft** is allowed: the quest is
//!   ready, and photographers see it when the journey is published — so a whole
//!   journey can be prepared and then go live at once.
//! * **Archived journeys** take no new quests and cannot have a quest published
//!   into them (`409 journey_archived`). Publishing an archived journey or quest
//!   brings it back.
//! * **Requirements** are fixed once a quest leaves `draft` (`409
//!   quest_published`) — a `PUT` carrying the same requirements is not a change.
//!   Title, description, XP and the time window stay editable.

use crate::bindings::auth::identity::types::Principal;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::listing::{self, Order};
use crate::moderation::{require_active, require_role};
use crate::progress::{
    data_of, doc, find, load, store_err, str_of, tri, u64_of, JOURNEYS, QUESTS,
};
use crate::{audit, introspect, is_admin, now_secs, Reply, Route};
use serde_json::{json, Map, Value};

pub fn handle(method: &Method, route: &Route, body: &str, path: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    if let Err(r) = require_role(&principal, "curator") {
        audit("curator.route", "deny", &principal.subject, "forbidden_role");
        return r;
    }
    if !matches!(method, Method::Get) {
        if let Err(r) = require_active(&principal) {
            return r;
        }
    }
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Get, ["api", "curator", "journeys"]) => list(JOURNEYS, "journeys", path),
        (Method::Post, ["api", "curator", "journeys"]) => create_journey(&principal, body),
        (Method::Get, ["api", "curator", "journeys", id]) => get_journey(id),
        (Method::Put, ["api", "curator", "journeys", id]) => update_journey(&principal, id, body),
        (Method::Post, ["api", "curator", "journeys", id, "publish"]) => {
            set_journey_state(&principal, id, "published")
        }
        (Method::Post, ["api", "curator", "journeys", id, "archive"]) => {
            set_journey_state(&principal, id, "archived")
        }
        (Method::Get, ["api", "curator", "quests"]) => list(QUESTS, "quests", path),
        (Method::Post, ["api", "curator", "quests"]) => create_quest(&principal, body),
        (Method::Get, ["api", "curator", "quests", id]) => get_quest(id),
        (Method::Put, ["api", "curator", "quests", id]) => update_quest(&principal, id, body),
        (Method::Post, ["api", "curator", "quests", id, "publish"]) => {
            set_quest_state(&principal, id, "published")
        }
        (Method::Post, ["api", "curator", "quests", id, "archive"]) => {
            set_quest_state(&principal, id, "archived")
        }
        _ => Reply::err(404, "not_found"),
    }
}

// ---- helpers ------------------------------------------------------------------

fn bad(code: &str, detail: impl Into<String>) -> Reply {
    Reply::json(400, json!({"error": code, "detail": detail.into()}))
}

fn parse_object(body: &str) -> Result<Map<String, Value>, Reply> {
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(Reply::err(400, "bad_json")),
    }
}

/// The creator, or an admin who also holds `curator` (already checked).
fn may_edit(principal: &Principal, journey: &Map<String, Value>) -> bool {
    str_of(journey, "created_by") == principal.subject || is_admin(principal)
}

fn forbidden(principal: &Principal, event: &str, id: &str) -> Reply {
    audit(event, "deny", &principal.subject, id);
    Reply::err(403, "forbidden")
}

/// A curator list (CONTRACT.md "Browsing and the archive"): `state` (default
/// `all`) through the `state` index, `q` over the title, newest created first,
/// keyset-paged.
pub(crate) fn list(collection: &str, key: &str, path: &str) -> Reply {
    let mut states: Vec<&str> = listing::STATES.to_vec();
    states.push("all");
    let state = tri!(listing::choice(path, "state", &states, "all"));
    let scope = format!("curator-{key}:{state}");
    let page = tri!(listing::page_of(path, &scope));
    let q = listing::search(path);
    let rows: Vec<Map<String, Value>> = tri!(listing::in_state(collection, &state))
        .into_iter()
        .filter(|m| listing::matches(str_of(m, "title"), q.as_deref()))
        .collect();
    let (rows, next) = listing::cut(
        rows,
        |m| listing::pos_of(m, &["created_at"]),
        Order::Desc,
        &page,
        &scope,
    );
    Reply::json(200, json!({ key: rows, "state": state, "next": next }))
}

fn save(
    collection: &str,
    entry: &records::Entry,
    m: &Map<String, Value>,
) -> Result<Map<String, Value>, Reply> {
    match records::update(collection, &entry.id, &data_of(m), entry.revision) {
        Ok(e) => Ok(doc(&e)),
        Err(records::StoreError::RevisionConflict(_)) => Err(Reply::err(409, "conflict")),
        Err(_) => Err(store_err()),
    }
}

/// A non-empty trimmed string field.
fn title_of(m: &Map<String, Value>) -> Result<String, Reply> {
    match m.get("title").and_then(Value::as_str).map(str::trim) {
        Some(t) if !t.is_empty() => Ok(t.to_string()),
        _ => Err(Reply::err(400, "title is required")),
    }
}

fn description_of(v: Option<&Value>) -> Result<Value, Reply> {
    match v {
        None | Some(Value::Null) => Ok(json!("")),
        Some(Value::String(s)) => Ok(json!(s)),
        Some(_) => Err(Reply::err(400, "description must be a string")),
    }
}

/// Levels: consecutive from 1, strictly ascending XP, first `{1, 0}`.
pub(crate) fn validate_levels(v: &Value) -> Result<Value, String> {
    let Some(arr) = v.as_array() else { return Err("levels must be an array".into()) };
    if arr.is_empty() {
        return Err("levels must not be empty".into());
    }
    let mut out = Vec::new();
    let mut prev_xp: Option<u64> = None;
    for (i, l) in arr.iter().enumerate() {
        let Some(o) = l.as_object() else { return Err(format!("levels[{i}] must be an object")) };
        if o.keys().any(|k| k != "level" && k != "xp") {
            return Err(format!("levels[{i}] has only level and xp"));
        }
        let (Some(level), Some(xp)) =
            (o.get("level").and_then(Value::as_u64), o.get("xp").and_then(Value::as_u64))
        else {
            return Err(format!("levels[{i}] needs whole-number level and xp"));
        };
        if level != i as u64 + 1 {
            return Err(format!("levels[{i}] must be level {}", i + 1));
        }
        if i == 0 && xp != 0 {
            return Err("the first level is always {level: 1, xp: 0}".into());
        }
        if prev_xp.is_some_and(|p| xp <= p) {
            return Err(format!("levels[{i}].xp must be above the level before it"));
        }
        prev_xp = Some(xp);
        out.push(json!({"level": level, "xp": xp}));
    }
    Ok(Value::Array(out))
}

fn badge_of(v: Option<&Value>) -> Result<Value, Reply> {
    match v {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(Value::Object(b)) => match b.get("name").and_then(Value::as_str).map(str::trim) {
            Some(n) if !n.is_empty() => Ok(json!({"name": n})),
            _ => Err(Reply::err(400, "badge.name is required")),
        },
        Some(_) => Err(Reply::err(400, "badge must be an object or null")),
    }
}

// ---- journeys -----------------------------------------------------------------

fn create_journey(principal: &Principal, body: &str) -> Reply {
    let req = tri!(parse_object(body));
    let title = tri!(title_of(&req));
    let description = tri!(description_of(req.get("description")));
    let levels = match req.get("levels") {
        None | Some(Value::Null) => json!([{"level": 1, "xp": 0}]),
        Some(v) => match validate_levels(v) {
            Ok(l) => l,
            Err(d) => return bad("bad_levels", d),
        },
    };
    let badge = tri!(badge_of(req.get("badge")));
    let rec = json!({
        "title": title, "description": description, "state": "draft",
        "created_by": principal.subject, "created_at": now_secs(),
        "levels": levels, "badge": badge, "quests": [],
    });
    let entry = match records::create(JOURNEYS, &rec.to_string(), &listing::index_fields(&["created_by", "state"])) {
        Ok(e) => e,
        Err(_) => return store_err(),
    };
    audit("journey.create", "allow", &principal.subject, &entry.id);
    Reply::json(201, Value::Object(doc(&entry)))
}

fn get_journey(id: &str) -> Reply {
    let Some((_, mut j)) = tri!(load(JOURNEYS, id)) else { return Reply::err(404, "not_found") };
    let mut docs = Vec::new();
    for qid in j.get("quests").and_then(Value::as_array).cloned().unwrap_or_default() {
        if let Some((_, q)) = tri!(load(QUESTS, qid.as_str().unwrap_or_default())) {
            docs.push(Value::Object(q));
        }
    }
    j.insert("quest_docs".into(), Value::Array(docs));
    Reply::json(200, Value::Object(j))
}

fn update_journey(principal: &Principal, id: &str, body: &str) -> Reply {
    let req = tri!(parse_object(body));
    let Some((entry, mut j)) = tri!(load(JOURNEYS, id)) else {
        return Reply::err(404, "not_found");
    };
    if !may_edit(principal, &j) {
        return forbidden(principal, "journey.update", id);
    }
    if req.contains_key("title") {
        j.insert("title".into(), json!(tri!(title_of(&req))));
    }
    if req.contains_key("description") {
        j.insert("description".into(), tri!(description_of(req.get("description"))));
    }
    if let Some(v) = req.get("levels") {
        match validate_levels(v) {
            Ok(l) => j.insert("levels".into(), l),
            Err(d) => return bad("bad_levels", d),
        };
    }
    if req.contains_key("badge") {
        j.insert("badge".into(), tri!(badge_of(req.get("badge"))));
    }
    if let Some(v) = req.get("quests") {
        let Some(new) = v.as_array().filter(|a| a.iter().all(Value::is_string)) else {
            return Reply::err(400, "quests must be a list of quest ids");
        };
        let mut want: Vec<&str> = new.iter().filter_map(Value::as_str).collect();
        let mut have: Vec<&str> = j
            .get("quests")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        want.sort_unstable();
        have.sort_unstable();
        if want != have {
            return Reply::err(
                400,
                "quests must reorder this journey's quests, not add or drop any",
            );
        }
        j.insert("quests".into(), v.clone());
    }
    j.insert("updated_at".into(), json!(now_secs()));
    let saved = tri!(save(JOURNEYS, &entry, &j));
    audit("journey.update", "allow", &principal.subject, id);
    Reply::json(200, Value::Object(saved))
}

fn set_journey_state(principal: &Principal, id: &str, state: &str) -> Reply {
    let Some((entry, mut j)) = tri!(load(JOURNEYS, id)) else {
        return Reply::err(404, "not_found");
    };
    if !may_edit(principal, &j) {
        return forbidden(principal, &format!("journey.{state}"), id);
    }
    if state == "published" {
        // Keep the order; append any quest of this journey the list is missing,
        // and drop ids that no longer name one of its quests.
        let mine = tri!(find(QUESTS, "journey", id));
        let mut order: Vec<String> = j
            .get("quests")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|q| mine.iter().any(|m| str_of(m, "id") == *q))
            .map(str::to_string)
            .collect();
        for m in &mine {
            let qid = str_of(m, "id").to_string();
            if !order.contains(&qid) {
                order.push(qid);
            }
        }
        j.insert("quests".into(), json!(order));
    }
    j.insert("state".into(), json!(state));
    j.insert(format!("{state}_at"), json!(now_secs()));
    let saved = tri!(save(JOURNEYS, &entry, &j));
    audit(&format!("journey.{state}"), "allow", &principal.subject, id);
    Reply::json(200, Value::Object(saved))
}

// ---- quests -------------------------------------------------------------------

/// The time window from `req` over `current`: `(starts_at, ends_at)`.
fn window_of(
    req: &Map<String, Value>,
    current: Option<&Map<String, Value>>,
) -> Result<(u64, Value), Reply> {
    let starts = match req.get("starts_at") {
        None => current.map(|c| u64_of(c, "starts_at")).unwrap_or_else(crate::clock::now),
        Some(Value::Null) => crate::clock::now(),
        Some(v) => v.as_u64().ok_or_else(|| Reply::err(400, "starts_at must be unix seconds"))?,
    };
    let ends = match req.get("ends_at") {
        None => current.and_then(|c| c.get("ends_at").cloned()).unwrap_or(Value::Null),
        Some(Value::Null) => Value::Null,
        Some(v) => json!(v
            .as_u64()
            .ok_or_else(|| Reply::err(400, "ends_at must be unix seconds or null"))?),
    };
    if ends.as_u64().is_some_and(|e| e <= starts) {
        return Err(Reply::err(400, "ends_at must be after starts_at"));
    }
    Ok((starts, ends))
}

fn xp_of(v: Option<&Value>) -> Result<u64, Reply> {
    match v {
        None | Some(Value::Null) => Ok(0),
        Some(v) => v.as_u64().ok_or_else(|| Reply::err(400, "xp must be a whole number ≥ 0")),
    }
}

fn requirements_of(v: Option<&Value>) -> Result<Value, Reply> {
    let r = match v {
        None | Some(Value::Null) => json!({}),
        Some(v) => v.clone(),
    };
    crate::rules::validate(&r).map_err(|d| bad("bad_requirements", d))?;
    Ok(r)
}

fn create_quest(principal: &Principal, body: &str) -> Reply {
    let req = tri!(parse_object(body));
    let journey_id = str_of(&req, "journey").trim().to_string();
    if journey_id.is_empty() {
        return Reply::err(400, "journey is required");
    }
    let title = tri!(title_of(&req));
    let description = tri!(description_of(req.get("description")));
    let xp = tri!(xp_of(req.get("xp")));
    let (starts_at, ends_at) = tri!(window_of(&req, None));
    let requirements = tri!(requirements_of(req.get("requirements")));
    let Some((_, j)) = tri!(load(JOURNEYS, &journey_id)) else {
        return Reply::err(400, "no such journey");
    };
    if !may_edit(principal, &j) {
        return forbidden(principal, "quest.create", &journey_id);
    }
    if str_of(&j, "state") == "archived" {
        return Reply::err(409, "journey_archived");
    }
    let rec = json!({
        "journey": journey_id, "title": title, "description": description, "xp": xp,
        "starts_at": starts_at, "ends_at": ends_at, "requirements": requirements,
        "state": "draft", "created_by": principal.subject, "created_at": now_secs(),
    });
    let entry = match records::create(QUESTS, &rec.to_string(), &listing::index_fields(&["journey", "state"])) {
        Ok(e) => e,
        Err(_) => return store_err(),
    };
    // Append to the journey's order; a concurrent edit of the journey is re-read.
    for attempt in 0..3 {
        let Some((je, mut j)) = tri!(load(JOURNEYS, &journey_id)) else { break };
        let mut order = j.get("quests").and_then(Value::as_array).cloned().unwrap_or_default();
        order.push(json!(entry.id));
        j.insert("quests".into(), Value::Array(order));
        match records::update(JOURNEYS, &je.id, &data_of(&j), je.revision) {
            Ok(_) => break,
            Err(records::StoreError::RevisionConflict(_)) if attempt < 2 => continue,
            // Publishing the journey appends any quest its list is missing, so a
            // quest left out here is not lost.
            Err(_) => break,
        }
    }
    audit("quest.create", "allow", &principal.subject, &entry.id);
    Reply::json(201, Value::Object(doc(&entry)))
}

fn get_quest(id: &str) -> Reply {
    match tri!(load(QUESTS, id)) {
        Some((_, q)) => Reply::json(200, Value::Object(q)),
        None => Reply::err(404, "not_found"),
    }
}

/// The quest and its journey, when `principal` may edit them.
fn editable_quest(
    principal: &Principal,
    id: &str,
    event: &str,
) -> Result<(records::Entry, Map<String, Value>, Map<String, Value>), Reply> {
    let Some((entry, q)) = load(QUESTS, id)? else { return Err(Reply::err(404, "not_found")) };
    let j = load(JOURNEYS, str_of(&q, "journey"))?.map(|(_, j)| j).unwrap_or_default();
    if !may_edit(principal, &j) && str_of(&q, "created_by") != principal.subject {
        return Err(forbidden(principal, event, id));
    }
    Ok((entry, q, j))
}

fn update_quest(principal: &Principal, id: &str, body: &str) -> Reply {
    let req = tri!(parse_object(body));
    let (entry, mut q, _) = tri!(editable_quest(principal, id, "quest.update"));
    if let Some(j) = req.get("journey") {
        if j.as_str() != Some(str_of(&q, "journey")) {
            return Reply::err(400, "a quest cannot move to another journey");
        }
    }
    if req.contains_key("requirements") {
        let r = tri!(requirements_of(req.get("requirements")));
        if str_of(&q, "state") != "draft" && Some(&r) != q.get("requirements") {
            audit("quest.update", "deny", &principal.subject, &format!("{id} quest_published"));
            return Reply::err(409, "quest_published");
        }
        q.insert("requirements".into(), r);
    }
    if req.contains_key("title") {
        q.insert("title".into(), json!(tri!(title_of(&req))));
    }
    if req.contains_key("description") {
        q.insert("description".into(), tri!(description_of(req.get("description"))));
    }
    if req.contains_key("xp") {
        q.insert("xp".into(), json!(tri!(xp_of(req.get("xp")))));
    }
    let (starts_at, ends_at) = tri!(window_of(&req, Some(&q)));
    q.insert("starts_at".into(), json!(starts_at));
    q.insert("ends_at".into(), ends_at);
    q.insert("updated_at".into(), json!(now_secs()));
    let saved = tri!(save(QUESTS, &entry, &q));
    audit("quest.update", "allow", &principal.subject, id);
    Reply::json(200, Value::Object(saved))
}

fn set_quest_state(principal: &Principal, id: &str, state: &str) -> Reply {
    let event = format!("quest.{state}");
    let (entry, mut q, j) = tri!(editable_quest(principal, id, &event));
    if state == "published" && str_of(&j, "state") == "archived" {
        return Reply::err(409, "journey_archived");
    }
    q.insert("state".into(), json!(state));
    q.insert(format!("{state}_at"), json!(now_secs()));
    let saved = tri!(save(QUESTS, &entry, &q));
    audit(&event, "allow", &principal.subject, id);
    Reply::json(200, Value::Object(saved))
}

#[cfg(test)]
mod tests {
    use super::validate_levels;
    use serde_json::json;

    #[test]
    fn levels_start_at_one_zero_and_ascend() {
        assert!(validate_levels(&json!([{"level": 1, "xp": 0}])).is_ok());
        assert!(validate_levels(
            &json!([{"level": 1, "xp": 0}, {"level": 2, "xp": 100}, {"level": 3, "xp": 250}])
        )
        .is_ok());
        for bad in [
            json!([]),
            json!({}),
            json!([{"level": 1, "xp": 10}]),
            json!([{"level": 2, "xp": 0}]),
            json!([{"level": 1, "xp": 0}, {"level": 2, "xp": 0}]),
            json!([{"level": 1, "xp": 0}, {"level": 3, "xp": 100}]),
            json!([{"level": 1, "xp": 0}, {"level": 2, "xp": 100}, {"level": 3, "xp": 50}]),
            json!([{"level": 1, "xp": 0, "name": "x"}]),
            json!([{"level": 1, "xp": -1}]),
        ] {
            assert!(validate_levels(&bad).is_err(), "should refuse {bad}");
        }
    }
}
