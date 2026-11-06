//! `gcal` — Google Calendar for agents: list, add, change and delete events through Google's
//! Calendar API, with the person's own login (ADR-0125).
//!
//! * The agent tool is `holon:tool/tool.call` with `{"op": "calendars" | "list" | "add" | "update"
//!   | "delete", ...}`, over wRPC, or `POST /tool` for a `url` connector.
//! * Signing in is once, in a browser: `GET /auth/start` sends it to Google (PKCE), and Google sends
//!   it back to `/auth/callback`, which keeps the refresh token in this app's keyvalue.
//!
//! Secret: `gcal-client-secret`.
//!
//! Config (wasi:config/store):
//!   gcal-client-id      the OAuth client's id, from Google Cloud
//!   gcal-redirect-uri   this app's /auth/callback, as registered with the OAuth client
//!   gcal-utc-offset     the local offset dates are in, e.g. +01:00 (a component has no time zone)
//!
//! An event's description is left out of results on purpose: anyone who invited you wrote it, and
//! handing it to a model is how a calendar invite becomes an instruction.

#[allow(warnings)]
mod bindings;
mod clock;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use bindings::comp::secrets::reader as secrets;
use bindings::exports::holon::tool::tool::Guest as Tool;
use bindings::exports::wasi::http::incoming_handler::Guest as Http;
use bindings::wasi::config::store as config;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingRequest, OutgoingResponse,
    RequestOptions, ResponseOutparam, Scheme,
};
use bindings::wasi::keyvalue::store;

struct Component;

/// Read and write calendars and events; no mail, no drive.
const SCOPE: &str = "https://www.googleapis.com/auth/calendar";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const MAX_BODY_BYTES: usize = 1024 * 1024;

guestio::guest_write_all!();
guestio::guest_read_body_text!(MAX_BODY_BYTES);

fn cfg(key: &str, default: &str) -> String {
    config::get(key).ok().flatten().filter(|s| !s.is_empty()).unwrap_or_else(|| default.into())
}

fn api_base() -> String {
    cfg("gcal-api-base", "https://www.googleapis.com/calendar/v3")
}

fn token_url() -> String {
    cfg("gcal-token-url", "https://oauth2.googleapis.com/token")
}

fn client() -> Result<(String, String), String> {
    let id = cfg("gcal-client-id", "");
    let secret = match secrets::get("gcal-client-secret") {
        Ok(Some(s)) => secrets::reveal(&s).unwrap_or_default(),
        _ => String::new(),
    };
    if id.is_empty() || secret.is_empty() {
        return Err("Google Calendar is not set up: the app needs `gcal-client-id` in its config \
                    and a `gcal-client-secret` secret (an OAuth client from Google Cloud)"
            .into());
    }
    Ok((id, secret))
}

fn bucket() -> Result<store::Bucket, String> {
    store::open("default").map_err(|e| format!("no keyvalue: {e:?}"))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A value in a URL query or path segment.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}

fn query(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect::<Vec<_>>().join("&")
}

/// One outgoing request; its status and body.
fn fetch(
    method: Method,
    url: &str,
    headers: &[(&str, String)],
    body: Option<(&str, Vec<u8>)>,
) -> Result<(u16, Vec<u8>), String> {
    let net = |m: String| format!("could not reach Google: {m}");
    let (scheme, rest) = match url.split_once("://") {
        Some(("https", r)) => (Scheme::Https, r),
        Some(("http", r)) => (Scheme::Http, r),
        _ => return Err(format!("not an http(s) url: {url}")),
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let fields = Fields::new();
    for (k, v) in headers {
        let _ = fields.set(k, &[v.as_bytes().to_vec()]);
    }
    if let Some((ctype, _)) = &body {
        let _ = fields.set("content-type", &[ctype.as_bytes().to_vec()]);
    }
    let req = OutgoingRequest::new(fields);
    req.set_method(&method).map_err(|_| net("method".into()))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("scheme".into()))?;
    req.set_authority(Some(authority)).map_err(|_| net("authority".into()))?;
    req.set_path_with_query(Some(if path.is_empty() { "/" } else { path }))
        .map_err(|_| net("path".into()))?;
    let out = req.body().map_err(|_| net("body".into()))?;
    if let Some((_, bytes)) = &body {
        let stream = out.write().map_err(|_| net("body stream".into()))?;
        if !write_all(&stream, bytes) {
            return Err(net("writing the body".into()));
        }
    }
    OutgoingBody::finish(out, None).map_err(|_| net("finishing the body".into()))?;
    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(15_000_000_000));
    let _ = opts.set_first_byte_timeout(Some(30_000_000_000));
    let fut = bindings::wasi::http::outgoing_handler::handle(req, Some(opts))
        .map_err(|e| net(format!("{e:?}")))?;
    fut.subscribe().block();
    let resp = fut
        .get()
        .ok_or_else(|| net("no response".into()))?
        .map_err(|_| net("response taken".into()))?
        .map_err(|e| net(format!("{e:?}")))?;
    let status = resp.status();
    let incoming = resp.consume().map_err(|_| net("response body".into()))?;
    let stream = incoming.stream().map_err(|_| net("response stream".into()))?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(c) if c.is_empty() => break,
            Ok(c) => {
                buf.extend_from_slice(&c);
                if buf.len() > MAX_BODY_BYTES {
                    return Err(net(format!("Google answered more than {MAX_BODY_BYTES} bytes")));
                }
            }
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // A failed read is not the end: a truncated reply would parse into a wrong whole one.
            Err(e) => return Err(net(format!("reading: {e:?}"))),
        }
    }
    Ok((status, buf))
}

fn form(pairs: &[(&str, &str)]) -> Option<(&'static str, Vec<u8>)> {
    Some(("application/x-www-form-urlencoded", query(pairs).into_bytes()))
}

/// What Google said went wrong, from its OAuth or its API error shape.
fn why(v: &Value) -> &str {
    v["error_description"]
        .as_str()
        .or(v["error"]["message"].as_str())
        .or(v["error"].as_str())
        .unwrap_or("no reason given")
}

/// A valid access token: the cached one, or a fresh one from the saved login.
fn access_token(force: bool) -> Result<String, String> {
    let b = bucket()?;
    if !force {
        if let Ok(Some(raw)) = b.get("gcal:access") {
            let raw = String::from_utf8_lossy(&raw).into_owned();
            if let Some((token, until)) = raw.split_once('\n') {
                if until.parse::<u64>().is_ok_and(|u| u > now_secs()) {
                    return Ok(token.to_string());
                }
            }
        }
    }
    let refresh = match b.get("gcal:refresh") {
        Ok(Some(r)) => String::from_utf8_lossy(&r).into_owned(),
        _ => return Err("not signed in to Google: open this app's /auth/start in a browser".into()),
    };
    let (id, secret) = client()?;
    let (status, body) = fetch(
        Method::Post,
        &token_url(),
        &[],
        form(&[
            ("client_id", &id),
            ("client_secret", &secret),
            ("refresh_token", &refresh),
            ("grant_type", "refresh_token"),
        ]),
    )?;
    let v: Value = serde_json::from_slice(&body).unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!(
            "Google refused the saved login ({}): open this app's /auth/start again",
            why(&v)
        ));
    }
    let token = v["access_token"].as_str().ok_or("Google sent no access token")?.to_string();
    let until = now_secs() + v["expires_in"].as_u64().unwrap_or(3000).saturating_sub(60);
    let _ = b.set("gcal:access", format!("{token}\n{until}").as_bytes());
    Ok(token)
}

/// One Calendar API call. A 401 means the access token went stale: refreshed once, then repeated.
fn api(
    method: Method,
    path: &str,
    q: &[(&str, &str)],
    body: Option<&Value>,
) -> Result<Value, String> {
    let url = match q {
        [] => format!("{}{path}", api_base()),
        q => format!("{}{path}?{}", api_base(), query(q)),
    };
    for attempt in 0..2 {
        let token = access_token(attempt == 1)?;
        let payload = body.map(|b| ("application/json", b.to_string().into_bytes()));
        let (status, bytes) = fetch(
            match &method {
                Method::Get => Method::Get,
                Method::Post => Method::Post,
                Method::Patch => Method::Patch,
                Method::Delete => Method::Delete,
                _ => Method::Get,
            },
            &url,
            &[("authorization", format!("Bearer {token}"))],
            payload,
        )?;
        match status {
            401 if attempt == 0 => continue,
            401 => {
                return Err("Google Calendar refused a fresh login: open /auth/start again".into())
            }
            204 => return Ok(Value::Null),
            _ => {}
        }
        let v: Value = serde_json::from_slice(&bytes).unwrap_or_default();
        if (200..300).contains(&status) {
            return Ok(v);
        }
        return Err(format!("Google Calendar answered {status}: {}", why(&v)));
    }
    Err("Google Calendar kept refusing the login".into())
}

/// A local `YYYY-MM-DD[THH:MM]` as the RFC 3339 time with an offset that Google wants.
fn rfc3339(local: &str, offset: &str) -> Result<String, String> {
    let (days, minute) = clock::parse_local(local)
        .ok_or_else(|| format!("`{local}` is not a date: write 2026-10-15 or 2026-10-15T06:30"))?;
    Ok(format!("{}:00{offset}", clock::local_of(days, i64::from(minute))))
}

fn offset() -> String {
    cfg("gcal-utc-offset", "+00:00")
}

/// A Google event time (`dateTime` or `date`) as `(date, Some("HH:MM") | None for all-day)`.
fn when(t: &Value) -> Option<(String, Option<String>)> {
    if let Some(d) = t["dateTime"].as_str() {
        return Some((d.get(..10)?.to_string(), Some(d.get(11..16)?.to_string())));
    }
    t["date"].as_str().map(|d| (d.to_string(), None))
}

/// One event as a line: when, title, place, calendar, and the id a later call can use.
fn event_line(e: &Value, calendar: &str) -> Option<String> {
    if e["status"] == "cancelled" {
        return None;
    }
    let (sd, st) = when(&e["start"])?;
    let (ed, et) = when(&e["end"])?;
    let span = match (st, et) {
        (Some(st), Some(et)) if sd == ed => format!("{sd} {st}–{et}"),
        (Some(st), Some(et)) => format!("{sd} {st}–{ed} {et}"),
        // an all-day end is the day AFTER the last one
        _ => {
            let last = clock::parse_local(&ed).map(|(d, _)| clock::date_of(d - 1)).unwrap_or(ed);
            if last == sd {
                format!("{sd} (all day)")
            } else {
                format!("{sd}–{last} (all day)")
            }
        }
    };
    let place = e["location"]
        .as_str()
        .filter(|l| !l.is_empty())
        .map(|l| format!(" @ {l}"))
        .unwrap_or_default();
    Some(format!(
        "{span}  {}{place}  [{calendar}]  (id {})",
        e["summary"].as_str().unwrap_or("(no title)"),
        e["id"].as_str().unwrap_or("?")
    ))
}

struct Cal {
    id: String,
    name: String,
    writable: bool,
}

fn calendar_list() -> Result<Vec<Cal>, String> {
    let v = api(Method::Get, "/users/me/calendarList", &[], None)?;
    Ok(v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some(Cal {
                id: c["id"].as_str()?.to_string(),
                name: c["summaryOverride"].as_str().or(c["summary"].as_str())?.to_string(),
                writable: matches!(c["accessRole"].as_str(), Some("owner" | "writer")),
            })
        })
        .collect())
}

/// The calendar an argument names: its exact name or id, or the primary one when it is empty.
fn resolve(name: &str) -> Result<Cal, String> {
    let all = calendar_list()?;
    let primary = |c: &&Cal| c.id.contains('@') && !c.id.contains("group.");
    let found = if name.is_empty() || name.eq_ignore_ascii_case("primary") {
        all.iter().find(primary)
    } else {
        all.iter().find(|c| c.id == name || c.name.eq_ignore_ascii_case(name))
    };
    match found {
        Some(c) => Ok(Cal { id: c.id.clone(), name: c.name.clone(), writable: c.writable }),
        None => Err(format!(
            "no calendar named `{name}`; the calendars are: {}",
            all.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

fn writable(name: &str) -> Result<Cal, String> {
    let c = resolve(name)?;
    if c.writable {
        Ok(c)
    } else {
        Err(format!("the calendar `{}` is read-only", c.name))
    }
}

fn calendars() -> Result<String, String> {
    let all = calendar_list()?;
    if all.is_empty() {
        return Ok("no calendars".into());
    }
    Ok(all
        .iter()
        .map(|c| format!("{}{}", c.name, if c.writable { "" } else { " (read-only)" }))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Events from `from` to `to` (default a week), oldest first. `calendar` is a name, or `all`.
fn list(args: &Value) -> Result<String, String> {
    let from = arg(args, "from");
    let (fd, _) = clock::parse_local(from).ok_or("`from` is needed: a date like 2026-10-15")?;
    let to = match arg(args, "to") {
        "" => clock::date_of(fd + 7),
        t => t.to_string(),
    };
    let off = offset();
    let time_min = rfc3339(from, &off)?;
    // a bare date means "through the end of that day"
    let time_max = match clock::parse_local(&to) {
        Some((d, 0)) if !to.contains('T') => rfc3339(&clock::date_of(d + 1), &off)?,
        _ => rfc3339(&to, &off)?,
    };
    let which = arg(args, "calendar");
    let cals: Vec<Cal> = if which.eq_ignore_ascii_case("all") {
        calendar_list()?.into_iter().take(12).collect()
    } else {
        vec![resolve(which)?]
    };
    let mut lines = Vec::new();
    for c in &cals {
        let v = api(
            Method::Get,
            &format!("/calendars/{}/events", encode(&c.id)),
            &[
                ("singleEvents", "true"),
                ("orderBy", "startTime"),
                ("timeMin", &time_min),
                ("timeMax", &time_max),
                ("maxResults", "100"),
            ],
            None,
        )?;
        lines.extend(
            v["items"].as_array().into_iter().flatten().filter_map(|e| event_line(e, &c.name)),
        );
    }
    // the lines start with the date and time, so sorting the text sorts the day
    lines.sort();
    if lines.is_empty() {
        return Ok("no events".into());
    }
    Ok(lines.join("\n"))
}

/// An event body from `title`, `start`, `end`, `notes` and `location`. A date with no time is an
/// all-day event, and its `end` is the last day, as a person says it.
fn event_body(args: &Value, offset: &str) -> Result<Value, String> {
    let start = arg(args, "start");
    let (sd, sm) = clock::parse_local(start)
        .ok_or("`start` is needed: 2026-10-15T06:30, or a date alone for an all-day event")?;
    let mut body = json!({"summary": arg(args, "title")});
    if start.contains('T') {
        let end = match arg(args, "end") {
            "" => clock::local_of(sd, i64::from(sm) + 60),
            e => e.to_string(),
        };
        if let Some((ed, em)) = clock::parse_local(&end) {
            if (ed, em) <= (sd, sm) {
                return Err("`end` must be after `start`".into());
            }
        }
        body["start"] = json!({"dateTime": rfc3339(start, offset)?});
        body["end"] = json!({"dateTime": rfc3339(&end, offset)?});
    } else {
        let last = match arg(args, "end") {
            "" => start.to_string(),
            e => e.to_string(),
        };
        let (ld, _) = clock::parse_local(&last).ok_or("`end` is not a date")?;
        if ld < sd {
            return Err("`end` must not be before `start`".into());
        }
        body["start"] = json!({"date": start});
        body["end"] = json!({"date": clock::date_of(ld + 1)});
    }
    for (k, field) in [("notes", "description"), ("location", "location")] {
        if !arg(args, k).is_empty() {
            body[field] = json!(arg(args, k));
        }
    }
    Ok(body)
}

fn add(args: &Value) -> Result<String, String> {
    if arg(args, "title").is_empty() {
        return Err("`title` is needed".into());
    }
    let body = event_body(args, &offset())?;
    let cal = writable(arg(args, "calendar"))?;
    let v = api(
        Method::Post,
        &format!("/calendars/{}/events", encode(&cal.id)),
        &[("sendUpdates", "none")],
        Some(&body),
    )?;
    let line = event_line(&v, &cal.name).unwrap_or_else(|| arg(args, "title").to_string());
    Ok(format!("added: {line}"))
}

fn event(cal: &Cal, id: &str) -> Result<Value, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("`id` is needed: the id at the end of an event line from `list`".into());
    }
    api(Method::Get, &format!("/calendars/{}/events/{id}", encode(&cal.id)), &[], None)
}

/// The new `start`/`end` for an update: a timed event keeps its length and an all-day one its
/// number of days unless `end` is given too.
fn moved(current: &Value, args: &Value) -> Result<Value, String> {
    let all_day = current["start"]["date"].is_string();
    let (Some((csd, cst)), Some((ced, cet))) = (when(&current["start"]), when(&current["end"]))
    else {
        return Err("that event has no readable time".into());
    };
    let start = match arg(args, "start") {
        "" if all_day => csd.clone(),
        "" => format!("{csd}T{}", cst.clone().unwrap_or_default()),
        s => s.to_string(),
    };
    if start.contains('T') == all_day {
        return Err(
            "an event cannot change between all-day and timed: delete it and add it again".into()
        );
    }
    let length = |a: &str, b: &str, c: &str| match (
        clock::parse_local(a),
        clock::parse_local(b),
        clock::parse_local(c),
    ) {
        (Some(o), Some(n), Some(oe)) => Ok((o, n, oe)),
        _ => Err("could not read the event's length".to_string()),
    };
    let end = if !arg(args, "end").is_empty() {
        arg(args, "end").to_string()
    } else if !all_day {
        let ((od, om), (nd, nm), (oed, oem)) = length(
            &format!("{csd}T{}", cst.unwrap_or_default()),
            &start,
            &format!("{ced}T{}", cet.unwrap_or_default()),
        )?;
        let minutes = (oed - od) * 1440 + i64::from(oem) - i64::from(om);
        clock::local_of(nd, i64::from(nm) + minutes)
    } else {
        let ((od, _), (nd, _), (oed, _)) = length(&csd, &start, &ced)?;
        clock::date_of(nd + (oed - od) - 1)
    };
    Ok(json!({"title": "", "start": start, "end": end}))
}

fn update(args: &Value) -> Result<String, String> {
    let cal = writable(arg(args, "calendar"))?;
    let id = arg(args, "id");
    let current = event(&cal, id)?;
    let mut patch = json!({});
    if !arg(args, "title").is_empty() {
        patch["summary"] = json!(arg(args, "title"));
    }
    for (k, field) in [("notes", "description"), ("location", "location")] {
        if !arg(args, k).is_empty() {
            patch[field] = json!(arg(args, k));
        }
    }
    if !arg(args, "start").is_empty() || !arg(args, "end").is_empty() {
        let body = event_body(&moved(&current, args)?, &offset())?;
        patch["start"] = body["start"].clone();
        patch["end"] = body["end"].clone();
    }
    if patch.as_object().is_some_and(|o| o.is_empty()) {
        return Err("nothing to change: give a title, start, end, notes or location".into());
    }
    let v = api(
        Method::Patch,
        &format!("/calendars/{}/events/{id}", encode(&cal.id)),
        &[("sendUpdates", "none")],
        Some(&patch),
    )?;
    Ok(format!("updated: {}", event_line(&v, &cal.name).unwrap_or_else(|| id.to_string())))
}

fn delete(args: &Value) -> Result<String, String> {
    let cal = writable(arg(args, "calendar"))?;
    let id = arg(args, "id");
    let line = event_line(&event(&cal, id)?, &cal.name).unwrap_or_else(|| id.to_string());
    api(
        Method::Delete,
        &format!("/calendars/{}/events/{id}", encode(&cal.id)),
        &[("sendUpdates", "none")],
        None,
    )?;
    Ok(format!("deleted: {line}"))
}

fn call(args: &str) -> Result<String, String> {
    let args: Value = serde_json::from_str(if args.trim().is_empty() { "{}" } else { args })
        .map_err(|e| format!("the arguments are not JSON: {e}"))?;
    match arg(&args, "op") {
        "calendars" => calendars(),
        "list" => list(&args),
        "add" => add(&args),
        "update" => update(&args),
        "delete" => delete(&args),
        op => Err(format!("`op` must be calendars, list, add, update or delete, not `{op}`")),
    }
}

/// The consent URL: `access_type=offline` and `prompt=consent` so Google hands back a refresh
/// token every time, and PKCE so a code taken off the redirect is useless alone.
fn consent_url(client_id: &str, redirect: &str, state: &str, challenge: &str) -> String {
    format!(
        "{AUTH_URL}?{}",
        query(&[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", redirect),
            ("scope", SCOPE),
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("state", state),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ])
    )
}

fn start_sign_in() -> Result<String, String> {
    // Only the exchange in `finish_sign_in` needs the secret.
    let id = cfg("gcal-client-id", "");
    if id.is_empty() {
        return Err("`gcal-client-id` is not set: an OAuth client from Google Cloud".into());
    }
    let redirect = cfg("gcal-redirect-uri", "");
    if redirect.is_empty() {
        return Err("`gcal-redirect-uri` is not set: this app's /auth/callback, as registered \
                    with the OAuth client"
            .into());
    }
    let random = bindings::wasi::random::random::get_random_bytes;
    let verifier = B64.encode(random(48));
    let state = B64.encode(random(16));
    bucket()?
        .set(&format!("gcal:pkce:{state}"), verifier.as_bytes())
        .map_err(|e| format!("{e:?}"))?;
    let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));
    Ok(consent_url(&id, &redirect, &state, &challenge))
}

fn finish_sign_in(params: &[(String, String)]) -> Result<(), String> {
    let p = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    if let Some(e) = p("error") {
        return Err(format!("Google did not allow it: {e}"));
    }
    let (Some(code), Some(state)) = (p("code"), p("state")) else {
        return Err("Google sent no code".into());
    };
    let b = bucket()?;
    // One use: a replayed redirect finds nothing.
    let key = format!("gcal:pkce:{state}");
    let verifier = match b.get(&key) {
        Ok(Some(v)) => String::from_utf8_lossy(&v).into_owned(),
        _ => return Err("this sign-in was not started here, or was already used".into()),
    };
    let _ = b.delete(&key);
    let (id, secret) = client()?;
    let redirect = cfg("gcal-redirect-uri", "");
    let (status, body) = fetch(
        Method::Post,
        &token_url(),
        &[],
        form(&[
            ("code", code),
            ("client_id", &id),
            ("client_secret", &secret),
            ("redirect_uri", &redirect),
            ("grant_type", "authorization_code"),
            ("code_verifier", &verifier),
        ]),
    )?;
    let v: Value = serde_json::from_slice(&body).unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("Google refused the code: {}", why(&v)));
    }
    let refresh = v["refresh_token"].as_str().ok_or(
        "Google sent no refresh token: remove the app's access at \
         myaccount.google.com/permissions and sign in again",
    )?;
    b.set("gcal:refresh", refresh.as_bytes()).map_err(|e| format!("{e:?}"))?;
    let _ = b.delete("gcal:access");
    Ok(())
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn params(q: &str) -> Vec<(String, String)> {
    q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (decode(k), decode(v))).collect()
}

fn respond(out: ResponseOutparam, status: u16, headers: &[(&str, &str)], body: &str) {
    let fields = Fields::new();
    for (k, v) in headers {
        let _ = fields.set(k, &[v.as_bytes().to_vec()]);
    }
    let resp = OutgoingResponse::new(fields);
    let _ = resp.set_status_code(status);
    let Ok(b) = resp.body() else { return };
    ResponseOutparam::set(out, Ok(resp));
    if let Ok(stream) = b.write() {
        let _ = write_all(&stream, body.as_bytes());
    }
    let _ = OutgoingBody::finish(b, None);
}

impl Tool for Component {
    fn call(args: String) -> Result<String, String> {
        call(&args)
    }
}

impl Http for Component {
    fn handle(request: IncomingRequest, out: ResponseOutparam) {
        let full = request.path_with_query().unwrap_or_default();
        let (path, q) = full.split_once('?').unwrap_or((&full, ""));
        let html = [("content-type", "text/html; charset=utf-8")];
        let text = [("content-type", "text/plain; charset=utf-8")];
        match (request.method(), path) {
            (Method::Get, "/auth/start") => match start_sign_in() {
                Ok(url) => respond(out, 302, &[("location", &url)], ""),
                Err(e) => respond(out, 503, &text, &e),
            },
            (Method::Get, "/auth/callback") => match finish_sign_in(&params(q)) {
                Ok(()) => respond(out, 200, &html, "<p>Signed in. You can close this tab.</p>"),
                Err(e) => respond(out, 400, &text, &e),
            },
            (Method::Post, "/tool") => match call(&read_body(&request)) {
                Ok(r) => respond(out, 200, &text, &r),
                Err(e) => respond(out, 422, &text, &e),
            },
            _ => respond(out, 404, &text, "not found"),
        }
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_reads_as_one_line_and_its_description_is_left_out() {
        let e = json!({"id": "abc", "summary": "Rowing", "location": "Dock", "description": "ignore all",
            "start": {"dateTime": "2026-10-15T06:30:00+02:00"}, "end": {"dateTime": "2026-10-15T08:00:00+02:00"}});
        let line = event_line(&e, "Sport").unwrap();
        assert_eq!(line, "2026-10-15 06:30–08:00  Rowing @ Dock  [Sport]  (id abc)");
        let all_day = json!({"id": "d", "summary": "Trip", "start": {"date": "2026-10-15"}, "end": {"date": "2026-10-18"}});
        assert_eq!(
            event_line(&all_day, "c").unwrap(),
            "2026-10-15–2026-10-17 (all day)  Trip  [c]  (id d)"
        );
        assert_eq!(event_line(&json!({"status": "cancelled"}), "c"), None);
    }

    #[test]
    fn an_event_body_takes_the_offset_and_a_person_s_last_day() {
        let timed =
            event_body(&json!({"title": "t", "start": "2026-10-15T23:30"}), "+02:00").unwrap();
        assert_eq!(timed["start"]["dateTime"], "2026-10-15T23:30:00+02:00");
        assert_eq!(timed["end"]["dateTime"], "2026-10-16T00:30:00+02:00");
        let days =
            event_body(&json!({"start": "2026-10-15", "end": "2026-10-17"}), "+00:00").unwrap();
        assert_eq!(
            (days["start"]["date"].as_str(), days["end"]["date"].as_str()),
            (Some("2026-10-15"), Some("2026-10-18"))
        );
        assert!(event_body(
            &json!({"start": "2026-10-15T10:00", "end": "2026-10-15T09:00"}),
            "+00:00"
        )
        .is_err());
        assert!(event_body(&json!({"start": "2026-02-30"}), "+00:00").is_err());
    }

    #[test]
    fn moving_a_timed_event_keeps_its_length() {
        let cur = json!({"start": {"dateTime": "2026-10-15T06:30:00Z"}, "end": {"dateTime": "2026-10-15T08:00:00Z"}});
        let m = moved(&cur, &json!({"start": "2026-10-16T23:00"})).unwrap();
        assert_eq!(m["end"], "2026-10-17T00:30");
        let all_day = json!({"start": {"date": "2026-10-15"}, "end": {"date": "2026-10-17"}});
        assert_eq!(moved(&all_day, &json!({"start": "2026-11-01"})).unwrap()["end"], "2026-11-02");
        assert!(moved(&cur, &json!({"start": "2026-10-16"})).is_err());
    }

    #[test]
    fn the_consent_url_asks_for_offline_access_with_pkce_and_a_query_round_trips() {
        let u = consent_url("id.apps", "https://cal.example.com/auth/callback", "st", "ch");
        assert!(u.starts_with(AUTH_URL));
        assert!(u.contains("redirect_uri=https%3A%2F%2Fcal.example.com%2Fauth%2Fcallback"));
        assert!(u.contains("access_type=offline") && u.contains("code_challenge_method=S256"));
        let p = params("code=4%2F0Ab+c&state=s%3D");
        assert_eq!(p, [("code".into(), "4/0Ab c".into()), ("state".into(), "s=".into())]);
        assert_eq!(encode("a@b.com#x"), "a%40b.com%23x");
    }

    #[test]
    fn an_unknown_op_or_bad_json_is_explained() {
        assert!(call("{\"op\": \"x\"}").unwrap_err().contains("calendars, list"));
        assert!(call("nope").unwrap_err().contains("not JSON"));
    }
}
