//! Moderation — CONTRACT.md "Moderation", plus the helpers every game module
//! calls (`require_role`, `require_active`, `is_hidden`) and the bootstrap admin.
//!
//! Collections this module owns:
//!
//! - `accounts` — `{subject, email, created_at}`, indexed by `subject`. Written at
//!   register (`auth:identity` has no "list accounts"), read by `GET /api/admin/users`
//!   and to check that an admin action names a real account.
//! - `account_flags` — `{subject, suspended, reason, at, by}`, indexed by `subject`;
//!   at most one per subject.
//! - `reports` — `{photo, photo_owner, reporter, reason, note, state, created_at,
//!   resolved_at, resolved_by, resolution_note}`, indexed by `photo`, `state`, `reporter`.
//!
//! It reads `competition_entries` (`{competition, photo, owner, sha256}`, indexed by
//! `photo`) and `competitions` (`state`), which `competitions.rs` writes, to decide
//! whether a photo is visible to someone other than its owner.
//!
//! Roles live in `auth:identity/rbac`, not here. `auth-guard` re-resolves a session's
//! roles from the RBAC store on every `introspect`, so a grant or revoke takes effect
//! on the grantee's NEXT request with the token they already hold — no re-login.

use crate::bindings::auth::identity::rbac;
use crate::bindings::auth::identity::types::Principal;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::config::store as config;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, introspect, now_secs, Reply, Route, TENANT};
use serde_json::{json, Map, Value};

const PHOTOS: &str = "photos";
const ACCOUNTS: &str = "accounts";
const FLAGS: &str = "account_flags";
const REPORTS: &str = "reports";
const ENTRIES: &str = "competition_entries";
const COMPETITIONS: &str = "competitions";

const REPORT_REASONS: &[&str] = &["inappropriate", "stolen", "spam", "other"];
const REPORT_STATES: &[&str] = &["open", "dismissed", "actioned"];
/// The roles an admin may grant or revoke. `photographer` is everyone's and is
/// not an admin's to take away.
const GRANTABLE: &[&str] = &["curator", "admin"];

pub fn handle(method: &Method, route: &Route, body: &str, query: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "photos", id, "reports"]) => report(route, id, body),
        (_, ["api", "admin", rest @ ..]) => admin(method, route, rest, body, query),
        _ => Reply::err(404, "not_found"),
    }
}

// ---- helpers every game module calls ----

/// `Ok(())` when `principal` holds `role`, else the `403 forbidden_role` reply.
pub fn require_role(principal: &Principal, role: &str) -> Result<(), Reply> {
    if principal.roles.iter().any(|r| r == role) {
        Ok(())
    } else {
        Err(Reply::err(403, "forbidden_role"))
    }
}

/// `Err(403 suspended)` when the account is suspended. A store that cannot answer
/// is a 503, not a pass: a suspension that silently lapses whenever the store
/// hiccups is not a suspension.
pub fn require_active(principal: &Principal) -> Result<(), Reply> {
    match flags_of(&principal.subject) {
        Ok(Some((_, f))) if bool_of(&f, "suspended") => {
            audit("account.suspended", "deny", &principal.subject, "");
            Err(Reply::err(403, "suspended"))
        }
        Ok(_) => Ok(()),
        Err(_) => Err(Reply::err(503, "store_unavailable")),
    }
}

/// True when moderation has hidden this photo.
pub fn is_hidden(photo: &Map<String, Value>) -> bool {
    photo.get("moderation").and_then(|m| m.get("hidden")).and_then(Value::as_bool).unwrap_or(false)
}

// ---- register: the account book and the bootstrap admin ----

/// `POST /register` — the `guestauth` register, then two things it cannot do: keep
/// `{subject, email}` for the admin user list, and grant `admin` to the account
/// whose email is config `bootstrap-admin-email`. Nothing in the body can ask for
/// a role; the macro already ignores `role` (ROLES is `["photographer"]`).
pub fn register(body: &str) -> Reply {
    let mut reply = crate::register(body);
    if reply.status != 201 {
        return reply;
    }
    let subject = reply.json["subject"].as_str().unwrap_or_default().to_string();
    let email = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["email"].as_str().map(|s| s.trim().to_string()))
        .unwrap_or_default();
    let rec = json!({"subject": subject, "email": email, "created_at": now_secs()});
    let _ = records::create(ACCOUNTS, &rec.to_string(), &["subject".to_string()]);

    let bootstrap = match config::get("bootstrap-admin-email") {
        Ok(Some(v)) => v.trim().to_string(),
        _ => String::new(),
    };
    if !bootstrap.is_empty() && !email.is_empty() && email.eq_ignore_ascii_case(&bootstrap) {
        match rbac::assign_role(TENANT, &subject, "admin") {
            Ok(()) => audit(
                "role.grant",
                "allow",
                "bootstrap-admin-email",
                &json!({"target": subject, "role": "admin", "reason": "bootstrap-admin-email"})
                    .to_string(),
            ),
            Err(_) => audit("role.grant", "error", "bootstrap-admin-email", &subject),
        }
    }
    let roles = rbac::roles_for(TENANT, &subject).unwrap_or_default();
    if let Value::Object(m) = &mut reply.json {
        m.insert("roles".into(), json!(roles));
    }
    reply
}

// ---- small store helpers ----

fn doc(entry: &records::Entry) -> Map<String, Value> {
    let mut m = match serde_json::from_str::<Value>(&entry.data) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    m.insert("id".into(), json!(entry.id));
    m
}

fn str_of<'a>(m: &'a Map<String, Value>, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn bool_of(m: &Map<String, Value>, key: &str) -> bool {
    m.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn enc(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Write `m` over `entry` at the revision it was read at (`id` stripped).
fn save(collection: &str, entry: &records::Entry, m: &Map<String, Value>) -> Result<(), Reply> {
    let mut data = m.clone();
    data.remove("id");
    match records::update(collection, &entry.id, &Value::Object(data).to_string(), entry.revision) {
        Ok(_) => Ok(()),
        Err(records::StoreError::RevisionConflict(_)) => Err(Reply::err(409, "conflict")),
        Err(_) => Err(Reply::err(500, "store_error")),
    }
}

fn flags_of(subject: &str) -> Result<Option<(records::Entry, Map<String, Value>)>, ()> {
    let found = records::find_by(FLAGS, "subject", &enc(subject)).map_err(|_| ())?;
    Ok(found.into_iter().next().map(|e| {
        let m = doc(&e);
        (e, m)
    }))
}

fn account_exists(subject: &str) -> Result<bool, Reply> {
    match records::find_by(ACCOUNTS, "subject", &enc(subject)) {
        Ok(v) => Ok(!v.is_empty()),
        Err(_) => Err(Reply::err(500, "store_error")),
    }
}

fn all(collection: &str) -> Result<Vec<records::Entry>, Reply> {
    let mut out = Vec::new();
    let mut after = String::new();
    loop {
        let page = records::list_records(collection, 0, &after)
            .map_err(|_| Reply::err(500, "store_error"))?;
        out.extend(page.entries);
        if page.next.is_empty() {
            return Ok(out);
        }
        after = page.next;
    }
}

/// Is `photo` visible to someone other than its owner? Yes iff it is not hidden
/// and it is entered in a `published` competition — the leaderboard is the only
/// shared surface there is (CONTRACT.md "Leaderboard").
fn visible_to_others(photo_id: &str, photo: &Map<String, Value>) -> bool {
    if is_hidden(photo) {
        return false;
    }
    let Ok(entries) = records::find_by(ENTRIES, "photo", &enc(photo_id)) else {
        return false;
    };
    entries.iter().any(|e| {
        let entry = doc(e);
        records::get(COMPETITIONS, str_of(&entry, "competition"))
            .map(|c| str_of(&doc(&c), "state") == "published")
            .unwrap_or(false)
    })
}

// ---- reports ----

#[derive(serde::Deserialize)]
struct ReportReq {
    #[serde(default)]
    reason: String,
    #[serde(default)]
    note: Option<String>,
}

/// `POST /api/photos/{id}/reports`.
fn report(route: &Route, id: &str, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    if let Err(r) = require_active(&principal) {
        return r;
    }
    let req = guestauth::guest_parse_body!(body, ReportReq);
    if !REPORT_REASONS.contains(&req.reason.as_str()) {
        return Reply::json(
            400,
            json!({"error": "bad_reason", "detail": "reason must be inappropriate|stolen|spam|other"}),
        );
    }
    if !valid_id(id) {
        return Reply::err(404, "not_found");
    }
    let entry = guestauth::guest_get_or_404!(PHOTOS, id);
    let photo = doc(&entry);
    let owner = str_of(&photo, "owner").to_string();
    if owner == principal.subject {
        return Reply::err(400, "own_photo");
    }
    // Not visible to you is indistinguishable from not existing: a 404, so a
    // report cannot be used to probe which photo ids exist.
    if !visible_to_others(id, &photo) {
        audit("photo.report", "deny", &principal.subject, id);
        return Reply::err(404, "not_found");
    }
    let mine = match records::find_by(REPORTS, "photo", &enc(id)) {
        Ok(v) => v,
        Err(_) => return Reply::err(500, "store_error"),
    };
    if mine
        .iter()
        .map(doc)
        .any(|r| str_of(&r, "reporter") == principal.subject && str_of(&r, "state") == "open")
    {
        return Reply::err(409, "already_reported");
    }
    let data = json!({
        "photo": id, "photo_owner": owner, "reporter": principal.subject,
        "reason": req.reason, "note": req.note, "state": "open", "created_at": now_secs(),
        "resolved_at": null, "resolved_by": null, "resolution_note": null,
    });
    let idx = ["photo".to_string(), "state".to_string(), "reporter".to_string()];
    let created = match records::create(REPORTS, &data.to_string(), &idx) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_error"),
    };
    audit(
        "photo.report",
        "allow",
        &principal.subject,
        &json!({"target": id, "report": created.id, "reason": req.reason}).to_string(),
    );
    Reply::json(201, Value::Object(doc(&created)))
}

// ---- admin ----

fn admin_audit(event: &str, actor: &Principal, target: &str, reason: &str) {
    audit(event, "allow", &actor.subject, &json!({"target": target, "reason": reason}).to_string());
}

fn query_param(query: &str, key: &str) -> Option<String> {
    let q = query.split_once('?').map(|(_, q)| q).unwrap_or("");
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then(|| v.to_string())
    })
}

#[derive(serde::Deserialize, Default)]
struct ReasonReq {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

fn parse_reason(body: &str) -> Result<ReasonReq, Reply> {
    if body.trim().is_empty() {
        return Ok(ReasonReq::default());
    }
    serde_json::from_str(body).map_err(|_| Reply::err(400, "bad_json"))
}

fn required_reason(body: &str) -> Result<String, Reply> {
    let r = parse_reason(body)?;
    match r.reason.map(|s| s.trim().to_string()) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(Reply::err(400, "reason is required")),
    }
}

fn admin(method: &Method, route: &Route, rest: &[&str], body: &str, query: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    if let Err(r) = require_role(&principal, "admin") {
        audit("admin.access", "deny", &principal.subject, &route.segments.join("/"));
        return r;
    }
    let out = match (method, rest) {
        (Method::Get, ["reports"]) => list_reports(query),
        (Method::Post, ["reports", id, "dismiss"]) => dismiss(&principal, id, body),
        (Method::Post, ["photos", id, "hide"]) => hide(&principal, id, body),
        (Method::Post, ["photos", id, "unhide"]) => unhide(&principal, id, body),
        (Method::Post, ["users", id, "suspend"]) => suspend(&principal, id, body, true),
        (Method::Post, ["users", id, "unsuspend"]) => suspend(&principal, id, body, false),
        (Method::Post, ["users", id, "roles"]) => roles(&principal, id, body),
        (Method::Get, ["users"]) => users(),
        _ => Err(Reply::err(404, "not_found")),
    };
    out.unwrap_or_else(|r| r)
}

/// `GET /api/admin/reports?state=open|dismissed|actioned` (default `open`), newest first.
fn list_reports(query: &str) -> Result<Reply, Reply> {
    let state =
        query_param(query, "state").filter(|s| !s.is_empty()).unwrap_or_else(|| "open".into());
    if !REPORT_STATES.contains(&state.as_str()) {
        return Err(Reply::err(400, "bad_state"));
    }
    let entries = records::find_by(REPORTS, "state", &enc(&state))
        .map_err(|_| Reply::err(500, "store_error"))?;
    let mut out: Vec<Map<String, Value>> = entries.iter().map(doc).collect();
    out.sort_by(|a, b| str_of(b, "id").cmp(str_of(a, "id")));
    Ok(Reply::json(200, json!({"reports": out})))
}

/// `POST /api/admin/reports/{id}/dismiss {note?}` — only an open report.
fn dismiss(actor: &Principal, id: &str, body: &str) -> Result<Reply, Reply> {
    let req = parse_reason(body)?;
    if !valid_id(id) {
        return Err(Reply::err(404, "not_found"));
    }
    let entry = records::get(REPORTS, id).map_err(|_| Reply::err(404, "not_found"))?;
    let mut m = doc(&entry);
    if str_of(&m, "state") != "open" {
        return Err(Reply::json(
            409,
            json!({"error": "report_closed", "state": str_of(&m, "state")}),
        ));
    }
    m.insert("state".into(), json!("dismissed"));
    m.insert("resolved_at".into(), json!(now_secs()));
    m.insert("resolved_by".into(), json!(actor.subject));
    m.insert("resolution_note".into(), json!(req.note));
    save(REPORTS, &entry, &m)?;
    admin_audit("report.dismiss", actor, id, req.note.as_deref().unwrap_or(""));
    Ok(Reply::json(200, Value::Object(m)))
}

/// Read-modify-write a photo, retrying a revision conflict (the callback or a
/// `complete` may be writing the same record).
fn update_photo(
    id: &str,
    f: impl Fn(&mut Map<String, Value>),
) -> Result<Map<String, Value>, Reply> {
    if !valid_id(id) {
        return Err(Reply::err(404, "not_found"));
    }
    for _ in 0..3 {
        let entry = records::get(PHOTOS, id).map_err(|_| Reply::err(404, "not_found"))?;
        let mut m = doc(&entry);
        f(&mut m);
        match save(PHOTOS, &entry, &m) {
            Ok(()) => return Ok(m),
            Err(r) if r.status == 409 => continue,
            Err(r) => return Err(r),
        }
    }
    Err(Reply::err(503, "busy"))
}

/// `POST /api/admin/photos/{id}/hide {reason}` — and every open report on it
/// becomes `actioned`.
fn hide(actor: &Principal, id: &str, body: &str) -> Result<Reply, Reply> {
    let reason = required_reason(body)?;
    let at = now_secs();
    let m = update_photo(id, |m| {
        m.insert(
            "moderation".into(),
            json!({"hidden": true, "reason": reason, "at": at, "by": actor.subject}),
        );
    })?;
    let mut actioned = 0;
    if let Ok(reports) = records::find_by(REPORTS, "photo", &enc(id)) {
        for e in reports {
            let mut r = doc(&e);
            if str_of(&r, "state") != "open" {
                continue;
            }
            r.insert("state".into(), json!("actioned"));
            r.insert("resolved_at".into(), json!(at));
            r.insert("resolved_by".into(), json!(actor.subject));
            r.insert("resolution_note".into(), json!(reason));
            if save(REPORTS, &e, &r).is_ok() {
                actioned += 1;
            }
        }
    }
    admin_audit("photo.hide", actor, id, &reason);
    Ok(Reply::json(
        200,
        json!({"id": id, "moderation": m.get("moderation"), "reports_actioned": actioned}),
    ))
}

/// `POST /api/admin/photos/{id}/unhide` — idempotent.
fn unhide(actor: &Principal, id: &str, body: &str) -> Result<Reply, Reply> {
    let req = parse_reason(body)?;
    update_photo(id, |m| {
        m.remove("moderation");
    })?;
    admin_audit("photo.unhide", actor, id, req.reason.as_deref().unwrap_or(""));
    Ok(Reply::json(200, json!({"id": id, "moderation": {"hidden": false}})))
}

/// `POST /api/admin/users/{id}/suspend {reason}` / `unsuspend` — idempotent.
fn suspend(actor: &Principal, subject: &str, body: &str, on: bool) -> Result<Reply, Reply> {
    let reason =
        if on { required_reason(body)? } else { parse_reason(body)?.reason.unwrap_or_default() };
    if !valid_id(subject) || !account_exists(subject)? {
        return Err(Reply::err(404, "not_found"));
    }
    let data = json!({
        "subject": subject, "suspended": on, "reason": if on { json!(reason) } else { Value::Null },
        "at": now_secs(), "by": actor.subject,
    });
    let saved = match flags_of(subject).map_err(|_| Reply::err(500, "store_error"))? {
        Some((e, _)) => records::update(FLAGS, &e.id, &data.to_string(), 0).map(|_| ()),
        None => records::create(FLAGS, &data.to_string(), &["subject".to_string()]).map(|_| ()),
    };
    saved.map_err(|_| Reply::err(500, "store_error"))?;
    admin_audit(if on { "account.suspend" } else { "account.unsuspend" }, actor, subject, &reason);
    Ok(Reply::json(200, json!({"subject": subject, "suspended": on})))
}

#[derive(serde::Deserialize)]
struct RolesReq {
    #[serde(default)]
    grant: Option<String>,
    #[serde(default)]
    revoke: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /api/admin/users/{id}/roles {grant|revoke: "curator"|"admin"}`.
fn roles(actor: &Principal, subject: &str, body: &str) -> Result<Reply, Reply> {
    let req: RolesReq = serde_json::from_str(body).map_err(|_| Reply::err(400, "bad_json"))?;
    let (grant, role) = match (&req.grant, &req.revoke) {
        (Some(r), None) => (true, r.clone()),
        (None, Some(r)) => (false, r.clone()),
        _ => return Err(Reply::err(400, "exactly one of grant or revoke is required")),
    };
    if !GRANTABLE.contains(&role.as_str()) {
        return Err(Reply::json(400, json!({"error": "bad_role", "detail": "curator or admin"})));
    }
    if !valid_id(subject) || !account_exists(subject)? {
        return Err(Reply::err(404, "not_found"));
    }
    if !grant && role == "admin" && subject == actor.subject {
        audit("role.revoke", "deny", &actor.subject, "own admin");
        return Err(Reply::err(409, "last_word"));
    }
    let done = if grant {
        rbac::assign_role(TENANT, subject, &role)
    } else {
        rbac::revoke_role(TENANT, subject, &role)
    };
    done.map_err(Reply::auth_err)?;
    let reason = req.reason.unwrap_or_default();
    audit(
        if grant { "role.grant" } else { "role.revoke" },
        "allow",
        &actor.subject,
        &json!({"target": subject, "role": role, "reason": reason}).to_string(),
    );
    let now = rbac::roles_for(TENANT, subject).unwrap_or_default();
    Ok(Reply::json(200, json!({"subject": subject, "roles": now})))
}

/// `GET /api/admin/users` — every account registered here, oldest first.
fn users() -> Result<Reply, Reply> {
    let accounts = all(ACCOUNTS)?;
    let out: Vec<Value> = accounts
        .iter()
        .map(doc)
        .map(|a| {
            let subject = str_of(&a, "subject").to_string();
            let roles = rbac::roles_for(TENANT, &subject).unwrap_or_default();
            let flags = flags_of(&subject).ok().flatten().map(|(_, f)| f);
            let suspended = flags.as_ref().map(|f| bool_of(f, "suspended")).unwrap_or(false);
            let reason = flags.filter(|_| suspended).and_then(|f| f.get("reason").cloned());
            json!({
                "subject": subject, "email": str_of(&a, "email"), "roles": roles,
                "suspended": suspended, "suspension_reason": reason,
                "created_at": a.get("created_at"),
            })
        })
        .collect();
    Ok(Reply::json(200, json!({"users": out})))
}

#[cfg(test)]
mod tests {
    use super::query_param;

    #[test]
    fn reads_a_query_parameter() {
        assert_eq!(
            query_param("/api/admin/reports?state=dismissed", "state").as_deref(),
            Some("dismissed")
        );
        assert_eq!(
            query_param("/api/admin/reports?x=1&state=open", "state").as_deref(),
            Some("open")
        );
        assert_eq!(query_param("/api/admin/reports", "state"), None);
    }
}
