//! `booked-domain` — a Calendly-lite booking service (docs/apps/BOOKED.md) as ONE composed
//! wasm HTTP component. Exports `wasi:http`; imports only WIT contracts: the
//! composed auth-guard (`auth:identity`), `records:store`, `lock:mutex` (the
//! no-double-book guarantee), `email:template` (confirmation), `ical:codec`
//! (.ics export) and `rrule:recur` (recurring bookings). No bespoke auth,
//! storage, locking, calendar format, or recurrence math.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Map, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::rbac;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::email::template::renderer as email;
use bindings::ical::codec::codec as ical;
use bindings::lock::mutex::mutex as lock;
use bindings::records::store::store as records;
use bindings::rrule::recur::recur as rrule;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "booked";
const PRODID: &str = "comp//booked";
const RESOURCES: &str = "resources";
const AVAIL: &str = "availability";
const BOOKINGS: &str = "bookings";
const USERS: &str = "users";
const CONFIRM_TMPL: &str = "booking-confirmation";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage(),
            (Method::Post, ["api", "register"]) => register(&request),
            (Method::Post, ["api", "login"]) => login(&request),
            (Method::Post, ["api", "logout"]) => logout(&request),
            (Method::Get, ["api", "me"]) => me(&request),

            (Method::Post, ["api", "resources"]) => create_resource(&request),
            (Method::Get, ["api", "resources"]) => list_resources(&request),
            (Method::Post, ["api", "resources", id, "availability"]) => {
                set_availability(&request, id)
            }
            (Method::Get, ["api", "resources", id, "availability"]) => {
                get_availability(&request, id)
            }
            (Method::Get, ["api", "resources", id, "slots"]) => slots(&request, id, &path),
            (Method::Get, ["api", "resources", id, "calendar.ics"]) => resource_ics(&request, id),

            (Method::Post, ["api", "bookings"]) => create_booking(&request),
            (Method::Get, ["api", "bookings"]) => list_bookings(&request, &path),
            (Method::Delete, ["api", "bookings", id]) => cancel_booking(&request, id),
            (Method::Get, ["api", "bookings", x]) if x.ends_with(".ics") => {
                booking_ics(&request, x.trim_end_matches(".ics"))
            }
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
    Auth(AuthError),
    // status, content-type, optional download filename, body.
    File(u16, String, Option<String>, Vec<u8>),
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "booked",
            "about": "Calendly-lite resource booking — no-double-book via lock:mutex, recurring bookings (rrule:recur), .ics export (ical:codec), email confirmation",
            "auth": "POST /api/register|login|logout, GET /api/me",
            "owner": "POST /api/resources {key,name,slot?,tz?}, POST /api/resources/{id}/availability {windows:[{weekday,start,end}]}",
            "book": "GET /api/resources/{id}/slots?day=YYYY-MM-DD, POST /api/bookings {resource,day,start,end,note?,repeat?}",
            "export": "GET /api/bookings/{id}.ics, GET /api/resources/{id}/calendar.ics"
        })
        .to_string(),
    )
}

// ---- date helpers (Hinnant civil<->days; times are minutes-from-midnight) ----

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + if m <= 2 { 1 } else { 0 }, m as u32, d)
}

/// Parse `YYYY-MM-DD` to days-since-epoch; None if malformed.
fn day_to_days(s: &str) -> Option<i64> {
    let p: Vec<&str> = s.split('-').collect();
    if p.len() != 3 {
        return None;
    }
    let (y, m, d): (i64, i64, i64) = (p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

fn days_to_day(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// ISO weekday of a `YYYY-MM-DD`, Monday=0…Sunday=6 (None if malformed).
fn weekday(day: &str) -> Option<i64> {
    day_to_days(day).map(|dd| (((dd % 7) + 3) % 7 + 7) % 7)
}

/// Unix seconds for a `YYYY-MM-DD` at `min` minutes from midnight (UTC).
/// ponytail: times are emitted as UTC; a real deploy would carry each resource's
/// tz into DTSTART/DTEND. The resource's `tz` field is stored for that upgrade.
fn epoch(day: &str, min: i64) -> u64 {
    day_to_days(day).map(|dd| (dd * 86400 + min * 60).max(0) as u64).unwrap_or(0)
}

fn hhmm(min: i64) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

// ---- auth (auth-guard: auth:identity) ---------------------------------------

fn bearer(request: &IncomingRequest) -> Option<String> {
    let headers = request.headers();
    let vals = headers.get("authorization");
    let raw = vals.first()?;
    let s = String::from_utf8(raw.clone()).ok()?;
    s.strip_prefix("Bearer ").map(|t| t.to_string())
}

fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let token =
        bearer(request).ok_or(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())))?;
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

fn is_owner(p: &Principal) -> bool {
    p.roles.iter().any(|r| r == "owner")
}

/// The email we recorded for a subject at register (for confirmations/reports).
fn subject_email(subject: &str) -> String {
    records::find_by(USERS, "subject", &json!(subject).to_string())
        .ok()
        .and_then(|v| v.into_iter().next())
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
        .and_then(|v| v["email"].as_str().map(String::from))
        .unwrap_or_default()
}

fn register(request: &IncomingRequest) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    let p = match accounts::register(&email, &password, TENANT) {
        Ok(p) => p,
        Err(e) => return Outcome::Auth(e),
    };
    // demo self-assign of the global role (an admin would grant it in prod).
    let wanted = body["role"].as_str().unwrap_or("member");
    let role = if ["member", "owner"].contains(&wanted) { wanted } else { "member" };
    let _ = rbac::assign_role(&p.tenant, &p.subject, role);
    let u = json!({ "subject": p.subject, "email": email });
    let _ = records::create(USERS, &u.to_string(), &["subject".to_string(), "email".to_string()]);
    Outcome::Json(201, json!({ "subject": p.subject, "roles": [role] }).to_string())
}

fn login(request: &IncomingRequest) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    match accounts::login(&email, &password, TENANT) {
        Ok(tp) => Outcome::Json(
            200,
            json!({ "access_token": tp.access_token, "refresh_token": tp.refresh_token, "expires_in": tp.expires_in, "session_id": tp.session_id }).to_string(),
        ),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &IncomingRequest) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(
            200,
            json!({ "subject": p.subject, "roles": p.roles, "email": subject_email(&p.subject), "is_owner": is_owner(&p) }).to_string(),
        ),
        Err(o) => o,
    }
}

fn logout(request: &IncomingRequest) -> Outcome {
    let token = match bearer(request) {
        Some(t) => t,
        None => return Outcome::Auth(AuthError::InvalidToken("missing bearer".into())),
    };
    match session::revoke(&token) {
        Ok(()) => Outcome::Json(200, json!({ "ok": true }).to_string()),
        Err(e) => Outcome::Auth(e),
    }
}

// ---- resources + availability (owner-managed) -------------------------------

fn create_resource(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_owner(&p) {
        return Outcome::Err(403, "owner only".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let key = b["key"].as_str().unwrap_or("").trim().to_string();
    let name = b["name"].as_str().unwrap_or("").trim().to_string();
    if key.is_empty() || name.is_empty() {
        return Outcome::Err(422, "key and name required".into());
    }
    let slot = b["slot"].as_i64().filter(|s| *s > 0).unwrap_or(30);
    let tz = b["tz"].as_str().unwrap_or("UTC");
    let d = json!({ "id": Value::Null, "key": key, "name": name, "owner": p.subject, "slot": slot, "tz": tz, "created": now() });
    match records::create(RESOURCES, &d.to_string(), &["owner".to_string()]) {
        Ok(rec) => {
            let mut v: Value = serde_json::from_str(&rec.data).unwrap_or(d);
            v["id"] = json!(rec.id);
            Outcome::Json(201, v.to_string())
        }
        Err(e) => store_err(e),
    }
}

fn resource(id: &str) -> Option<Value> {
    records::get(RESOURCES, id).ok().and_then(|e| serde_json::from_str::<Value>(&e.data).ok()).map(
        |mut v| {
            v["id"] = json!(id);
            v
        },
    )
}

fn list_resources(request: &IncomingRequest) -> Outcome {
    if let Err(o) = introspect(request) {
        return o;
    }
    let items: Vec<Value> = records::list_records(RESOURCES, 1000, "")
        .map(|p| p.entries)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            serde_json::from_str::<Value>(&e.data).ok().map(|mut v| {
                v["id"] = json!(e.id);
                v
            })
        })
        .collect();
    Outcome::Json(200, json!({ "items": items }).to_string())
}

fn set_availability(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_owner(&p) {
        return Outcome::Err(403, "owner only".into());
    }
    if resource(id).is_none() {
        return Outcome::Err(404, "no such resource".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let windows = b["windows"].as_array().cloned().unwrap_or_default();
    // replace: delete existing windows for this resource, then recreate.
    for e in records::find_by(AVAIL, "resource", &json!(id).to_string()).unwrap_or_default() {
        let _ = records::delete(AVAIL, &e.id);
    }
    let mut saved = Vec::new();
    for w in windows {
        let (wd, s, e) = (
            w["weekday"].as_i64().unwrap_or(-1),
            w["start"].as_i64().unwrap_or(-1),
            w["end"].as_i64().unwrap_or(-1),
        );
        if !(0..7).contains(&wd) || s < 0 || e <= s || e > 1440 {
            continue;
        }
        let d = json!({ "id": Value::Null, "resource": id, "weekday": wd, "start": s, "end": e });
        if records::create(AVAIL, &d.to_string(), &["resource".to_string()]).is_ok() {
            saved.push(d);
        }
    }
    Outcome::Json(200, json!({ "windows": saved }).to_string())
}

fn availability(id: &str) -> Vec<Value> {
    let mut v: Vec<Value> = records::find_by(AVAIL, "resource", &json!(id).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .collect();
    v.sort_by_key(|w| (w["weekday"].as_i64().unwrap_or(0), w["start"].as_i64().unwrap_or(0)));
    v
}

fn get_availability(request: &IncomingRequest, id: &str) -> Outcome {
    if let Err(o) = introspect(request) {
        return o;
    }
    Outcome::Json(200, json!({ "windows": availability(id) }).to_string())
}

// ---- bookings ---------------------------------------------------------------

/// (start, end) of every booking for a resource on a day.
fn bookings_on(resource: &str, day: &str) -> Vec<(i64, i64)> {
    records::find_by(BOOKINGS, "resource", &json!(resource).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .filter(|v| v["day"].as_str() == Some(day))
        .map(|v| (v["start"].as_i64().unwrap_or(0), v["end"].as_i64().unwrap_or(0)))
        .collect()
}

/// Does [start,end) fit inside an availability window for `day`'s weekday? A
/// resource with NO windows at all is treated as always-available (usable out of
/// the box); once windows exist, a booking must fit one.
fn fits_availability(resource: &str, day: &str, start: i64, end: i64) -> bool {
    let all = availability(resource);
    if all.is_empty() {
        return true;
    }
    let Some(wd) = weekday(day) else { return false };
    all.iter().any(|w| {
        w["weekday"].as_i64() == Some(wd)
            && w["start"].as_i64().unwrap_or(0) <= start
            && end <= w["end"].as_i64().unwrap_or(0)
    })
}

fn overlaps(start: i64, end: i64, existing: &[(i64, i64)]) -> bool {
    existing.iter().any(|(s, e)| start < *e && *s < end)
}

/// Book ONE instance under a lock:mutex lease on `book:{resource}:{day}` — the
/// no-double-book critical section: acquire, re-check overlap, write, release.
/// Returns the stored booking, or None on conflict / lock contention.
fn book_one(
    res_id: &str,
    res_name: &str,
    p: &Principal,
    email_addr: &str,
    day: &str,
    start: i64,
    end: i64,
    note: &str,
) -> Option<Value> {
    let key = format!("book:{res_id}:{day}");
    // Brief spin: a competing booker holds the lease only for its tiny
    // check-then-write, so a bounded retry avoids spurious conflicts under load.
    let mut lease = None;
    for _ in 0..64 {
        match lock::acquire(&key, &p.subject, 10) {
            Ok(l) => {
                lease = Some(l);
                break;
            }
            Err(lock::LockError::Held(_)) => continue,
            Err(_) => return None,
        }
    }
    let lease = lease?;

    let result = if overlaps(start, end, &bookings_on(res_id, day)) {
        None
    } else {
        let d = json!({
            "id": Value::Null, "resource": res_id, "resource_name": res_name,
            "user": p.subject, "email": email_addr, "day": day,
            "start": start, "end": end, "note": note, "created": now()
        });
        records::create(BOOKINGS, &d.to_string(), &["resource".to_string(), "user".to_string()])
            .ok()
            .map(|rec| {
                let mut v: Value = serde_json::from_str(&rec.data).unwrap_or(d);
                v["id"] = json!(rec.id);
                v
            })
    };
    let _ = lock::release(&key, &lease.token);
    result
}

/// Render the booking-confirmation email (seeding a default template on first
/// use), returning the rendered subject + text for the SPA to show.
fn confirmation(email_addr: &str, res_name: &str, day: &str, start: i64, end: i64) -> Value {
    if email::get_template(CONFIRM_TMPL).is_err() {
        let _ = email::put_template(
            CONFIRM_TMPL,
            &email::Template {
                subject: "Booking confirmed: {resource} on {day}".into(),
                text: "Hi {email},\n\nYour booking of {resource} on {day} at {time} is confirmed.\n\nAdd it to your calendar with the .ics link. See you then!".into(),
                html: "<p>Hi {email},</p><p>Your booking of <b>{resource}</b> on <b>{day}</b> at <b>{time}</b> is confirmed.</p>".into(),
            },
        );
    }
    let vars = vec![
        email::Var { name: "email".into(), value: email_addr.into() },
        email::Var { name: "resource".into(), value: res_name.into() },
        email::Var { name: "day".into(), value: day.into() },
        email::Var { name: "time".into(), value: format!("{}–{}", hhmm(start), hhmm(end)) },
    ];
    match email::render(CONFIRM_TMPL, &vars) {
        Ok(m) => json!({ "subject": m.subject, "text": m.text }),
        Err(_) => Value::Null,
    }
}

fn create_booking(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let res_id = b["resource"].as_str().unwrap_or("").to_string();
    let res = match resource(&res_id) {
        Some(r) => r,
        None => return Outcome::Err(422, "unknown resource".into()),
    };
    let res_name = res["name"].as_str().unwrap_or("").to_string();
    let day = b["day"].as_str().unwrap_or("").to_string();
    let start = b["start"].as_i64().unwrap_or(-1);
    let end = b["end"].as_i64().unwrap_or(-1);
    if day_to_days(&day).is_none() {
        return Outcome::Err(422, "bad day (YYYY-MM-DD)".into());
    }
    if start < 0 || end <= start || end > 1440 {
        return Outcome::Err(422, "bad time range (minutes 0..1440, start<end)".into());
    }
    let note = b["note"].as_str().unwrap_or("");
    let email_addr = subject_email(&p.subject);

    // instance days: a single booking, or a recurrence expanded via rrule:recur.
    let days = match b.get("repeat").filter(|v| v.is_object()) {
        Some(rep) => expand_repeat(&day, rep),
        None => vec![day.clone()],
    };

    let mut booked = Vec::new();
    let mut conflicts = Vec::new();
    for d in &days {
        if !fits_availability(&res_id, d, start, end) {
            conflicts.push(format!("{d} (outside availability)"));
            continue;
        }
        match book_one(&res_id, &res_name, &p, &email_addr, d, start, end, note) {
            Some(rec) => booked.push(rec),
            None => conflicts.push(d.clone()),
        }
    }

    if booked.is_empty() {
        return Outcome::Err(409, format!("already booked (conflicts: {})", conflicts.join(", ")));
    }
    let first = &booked[0];
    let conf =
        confirmation(&email_addr, &res_name, first["day"].as_str().unwrap_or(&day), start, end);
    Outcome::Json(
        201,
        json!({ "booked": booked, "conflicts": conflicts, "confirmation": conf }).to_string(),
    )
}

/// Expand a `repeat` object ({freq, interval?, weekdays?, count?, until?}) into
/// instance days via rrule:recur, over a window from `day` to `until` (or +1yr).
fn expand_repeat(day: &str, rep: &Value) -> Vec<String> {
    let freq = match rep["freq"].as_str().unwrap_or("weekly") {
        "daily" => rrule::Freq::Daily,
        _ => rrule::Freq::Weekly,
    };
    let rule = rrule::Rule {
        frequency: freq,
        interval: rep["interval"].as_u64().unwrap_or(1) as u32,
        by_weekday: rep["weekdays"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_u64().map(|n| n as u8)).collect())
            .unwrap_or_default(),
        count: rep["count"].as_u64().unwrap_or(0) as u32,
        until: rep["until"].as_str().unwrap_or("").to_string(),
    };
    let window_to = if let Some(u) = rep["until"].as_str().filter(|s| !s.is_empty()) {
        u.to_string()
    } else {
        day_to_days(day).map(|d| days_to_day(d + 366)).unwrap_or_else(|| day.to_string())
    };
    match rrule::expand(day, &rule, day, &window_to) {
        Ok(v) if !v.is_empty() => v,
        _ => vec![day.to_string()],
    }
}

fn list_bookings(request: &IncomingRequest, path: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let from = query_str(path, "from").unwrap_or_else(|| "0000-00-00".into());
    let to = query_str(path, "to").unwrap_or_else(|| "9999-99-99".into());
    let all = if is_owner(&p) {
        records::list_records(BOOKINGS, 5000, "").map(|pg| pg.entries).unwrap_or_default()
    } else {
        records::find_by(BOOKINGS, "user", &json!(p.subject).to_string()).unwrap_or_default()
    };
    let mut items: Vec<Value> = all
        .iter()
        .filter_map(|e| {
            serde_json::from_str::<Value>(&e.data).ok().map(|mut v| {
                v["id"] = json!(e.id);
                v
            })
        })
        .filter(|v| {
            let d = v["day"].as_str().unwrap_or("");
            from.as_str() <= d && d <= to.as_str()
        })
        .collect();
    items.sort_by(|a, b| {
        (a["day"].as_str(), a["start"].as_i64()).cmp(&(b["day"].as_str(), b["start"].as_i64()))
    });
    Outcome::Json(200, json!({ "items": items }).to_string())
}

fn booking(id: &str) -> Option<Value> {
    records::get(BOOKINGS, id).ok().and_then(|e| serde_json::from_str::<Value>(&e.data).ok()).map(
        |mut v| {
            v["id"] = json!(id);
            v
        },
    )
}

fn cancel_booking(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let bk = match booking(id) {
        Some(b) => b,
        None => return Outcome::Err(404, "not_found".into()),
    };
    if !is_owner(&p) && bk["user"].as_str() != Some(&p.subject) {
        return Outcome::Err(403, "not your booking".into());
    }
    let _ = records::delete(BOOKINGS, id);
    Outcome::Json(200, json!({ "ok": true }).to_string())
}

// ---- .ics export (ical:codec) -----------------------------------------------

fn booking_event(bk: &Value) -> ical::Event {
    let day = bk["day"].as_str().unwrap_or("1970-01-01");
    let start = bk["start"].as_i64().unwrap_or(0);
    let end = bk["end"].as_i64().unwrap_or(0);
    let name = bk["resource_name"].as_str().unwrap_or("Booking");
    let note = bk["note"].as_str().unwrap_or("");
    ical::Event {
        uid: format!("{}@booked", bk["id"].as_str().unwrap_or("")),
        start: epoch(day, start),
        end: epoch(day, end),
        summary: name.to_string(),
        description: note.to_string(),
        location: String::new(),
        organizer: bk["email"].as_str().unwrap_or("").to_string(),
        rrule: String::new(),
        alarm_minutes: 15,
    }
}

fn booking_ics(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let bk = match booking(id) {
        Some(b) => b,
        None => return Outcome::Err(404, "not_found".into()),
    };
    if !is_owner(&p) && bk["user"].as_str() != Some(&p.subject) {
        return Outcome::Err(403, "not your booking".into());
    }
    let ics = ical::format_event(&booking_event(&bk), PRODID);
    Outcome::File(
        200,
        "text/calendar; charset=utf-8".into(),
        Some(format!("booking-{id}.ics")),
        ics.into_bytes(),
    )
}

fn resource_ics(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let res = match resource(id) {
        Some(r) => r,
        None => return Outcome::Err(404, "no such resource".into()),
    };
    // owner sees the whole feed; a member sees only their own on this resource.
    let owner = is_owner(&p);
    let events: Vec<ical::Event> = records::find_by(BOOKINGS, "resource", &json!(id).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            serde_json::from_str::<Value>(&e.data).ok().map(|mut v| {
                v["id"] = json!(e.id);
                v
            })
        })
        .filter(|v| owner || v["user"].as_str() == Some(&p.subject))
        .map(|v| booking_event(&v))
        .collect();
    let name = format!("{} — booked", res["name"].as_str().unwrap_or("resource"));
    let ics = ical::format_calendar(&events, PRODID, &name);
    // a feed is meant to be subscribed to (inline), not downloaded.
    Outcome::File(200, "text/calendar; charset=utf-8".into(), None, ics.into_bytes())
}

// ---- free-slot search -------------------------------------------------------

fn slots(request: &IncomingRequest, id: &str, path: &str) -> Outcome {
    if let Err(o) = introspect(request) {
        return o;
    }
    let res = match resource(id) {
        Some(r) => r,
        None => return Outcome::Err(404, "no such resource".into()),
    };
    let day = match query_str(path, "day") {
        Some(d) if day_to_days(&d).is_some() => d,
        _ => return Outcome::Err(422, "day=YYYY-MM-DD required".into()),
    };
    let slot = res["slot"].as_i64().filter(|s| *s > 0).unwrap_or(30);
    let wd = weekday(&day).unwrap_or(0);
    let booked = bookings_on(id, &day);

    // windows for this weekday; if the resource has no availability at all,
    // offer a default 09:00–17:00.
    let windows: Vec<(i64, i64)> = {
        let all = availability(id);
        if all.is_empty() {
            vec![(9 * 60, 17 * 60)]
        } else {
            all.iter()
                .filter(|w| w["weekday"].as_i64() == Some(wd))
                .map(|w| (w["start"].as_i64().unwrap_or(0), w["end"].as_i64().unwrap_or(0)))
                .collect()
        }
    };

    let mut free = Vec::new();
    for (ws, we) in windows {
        let mut t = ws;
        while t + slot <= we {
            if !overlaps(t, t + slot, &booked) {
                free.push(json!({ "start": t, "end": t + slot, "label": format!("{}–{}", hhmm(t), hhmm(t + slot)) }));
            }
            t += slot;
        }
    }
    Outcome::Json(200, json!({ "day": day, "slot": slot, "slots": free }).to_string())
}

// ---- http plumbing ----------------------------------------------------------

fn query_str(path: &str, key: &str) -> Option<String> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        if it.next()? == key {
            Some(it.next().unwrap_or("").replace("%3A", ":").replace("%2D", "-"))
        } else {
            None
        }
    })
}

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::Err(404, "not_found".into()),
        records::StoreError::InvalidJson(m) => Outcome::Err(422, m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let raw = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

/// The most a request body may be, before the component stops reading it.
///
/// There was no ceiling anywhere: 148 of 150 components accumulated whatever
/// arrived until the guest hit wasmtime's 64 MiB per-store memory cap and TRAPPED,
/// which reaches the caller as a closed connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);

fn emit(response_out: ResponseOutparam, result: Outcome) {
    if let Outcome::File(code, ctype, name, bytes) = result {
        let disp = name.map(|n| format!("attachment; filename=\"{}\"", n));
        return respond(response_out, code, &ctype, disp.as_deref(), &bytes);
    }
    let (code, body) = match result {
        Outcome::Json(c, b) => (c, b),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string()),
        Outcome::Auth(e) => {
            let msg = match &e {
                AuthError::InvalidToken(m) => m.clone(),
                AuthError::InvalidCredentials => "invalid credentials".into(),
                other => format!("{other:?}"),
            };
            (401, json!({ "error": msg }).to_string())
        }
        Outcome::File(..) => unreachable!(),
    };
    respond(response_out, code, "application/json", None, body.as_bytes());
}

fn respond(
    response_out: ResponseOutparam,
    status: u16,
    ctype: &str,
    disposition: Option<&str>,
    body: &[u8],
) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[ctype.as_bytes().to_vec()]);
    if let Some(d) = disposition {
        let _ = headers.set("content-disposition", &[d.as_bytes().to_vec()]);
    }
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
