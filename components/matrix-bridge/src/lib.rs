//! `matrix-bridge` — agent-matrix on comp-host: every agent a Matrix user, every project a Space.
//!
//! Synapse sends its appservice transactions here (`/_matrix/app/v1/...`); the host's pump
//! (`POST /internal/pump`, ADR-0130) makes Matrix match the runtime, posts approval polls and
//! delivers scheduled results and finished runs. A message starts a run as a runtime job, so no
//! request waits on an agent. The rooms the bridge made are one document in the app's keyvalue,
//! moved by compare-and-set.
//!
//! Config (wasi:config/store, each secret also from comp:secrets): the keys of
//! `agent_matrix::Config::entries` — `matrix-homeserver`, `matrix-server-name`, `matrix-owner`,
//! `runtime-url`, `history-chars`, and the secrets `matrix-as-token`, `matrix-hs-token`,
//! `matrix-admin-token`, `runtime-token`, `jev-key`. `holon matrix init` writes them all.

#[allow(warnings)]
mod bindings;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use agent_matrix::http::{Http, Response};
use agent_matrix::state::Store;
use agent_matrix::{appservice, Bridge, Config};
use serde_json::json;

use bindings::comp::secrets::reader as secrets;
use bindings::comp::store::cas;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::config::store as config;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingRequest, OutgoingResponse,
    RequestOptions, ResponseOutparam, Scheme,
};
use bindings::wasi::keyvalue::store;

struct Component;

/// A photo or a voice note is the largest thing that passes through.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
const STATE_KEY: &str = "bridge.state";
const PUMP_KEY: &str = "bridge.pump";
/// A pass the host's request timeout killed leaves its lease behind; past this it is taken over.
const LEASE_MS: u64 = 60_000;

guestio::guest_write_all!();
guestio::guest_read_body!(MAX_BODY_BYTES);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

/// A secret first, then the config file: the same key in either.
fn lookup(key: &str) -> Option<String> {
    match secrets::get(key) {
        Ok(Some(s)) => secrets::reveal(&s).ok().filter(|v| !v.is_empty()),
        _ => None,
    }
    .or_else(|| config::get(key).ok().flatten().filter(|v| !v.is_empty()))
}

fn bucket() -> Option<store::Bucket> {
    store::open("default").ok()
}

struct KvStore;

impl Store for KvStore {
    fn load(&self) -> (Option<Vec<u8>>, u64) {
        match bucket().map(|b| cas::get(&b, STATE_KEY)) {
            Some(Ok(Some(v))) => (Some(v.value), v.revision),
            _ => (None, 0),
        }
    }

    fn save(&self, body: &[u8], rev: u64) -> bool {
        bucket().is_some_and(|b| {
            matches!(cas::set(&b, STATE_KEY, body, rev), Ok(cas::Outcome::Committed(_)))
        })
    }
}

/// Two pump passes at once would both create a missing room: a pass takes the lease or skips.
fn take_lease() -> Option<u64> {
    let b = bucket()?;
    let (held, rev) = match cas::get(&b, PUMP_KEY).ok()? {
        Some(v) => (String::from_utf8_lossy(&v.value).parse::<u64>().unwrap_or(0), v.revision),
        None => (0, 0),
    };
    if held != 0 && now_ms().saturating_sub(held) < LEASE_MS {
        return None;
    }
    match cas::set(&b, PUMP_KEY, now_ms().to_string().as_bytes(), rev).ok()? {
        cas::Outcome::Committed(r) => Some(r),
        _ => None,
    }
}

fn release_lease(rev: u64) {
    if let Some(b) = bucket() {
        let _ = cas::set(&b, PUMP_KEY, b"0", rev);
    }
}

struct WasiHttp;

impl Http for WasiHttp {
    fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<Response, String> {
        let (scheme, rest) = match url.split_once("://") {
            Some(("https", r)) => (Scheme::Https, r),
            Some(("http", r)) => (Scheme::Http, r),
            _ => return Err(format!("not an http(s) url: {url}")),
        };
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let fields = Fields::new();
        for (k, v) in headers {
            let _ = fields.append(k, v.as_bytes());
        }
        let req = OutgoingRequest::new(fields);
        let e = |m: &str| m.to_string();
        let method = match method {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "DELETE" => Method::Delete,
            other => Method::Other(other.to_string()),
        };
        req.set_method(&method).map_err(|_| e("method"))?;
        req.set_scheme(Some(&scheme)).map_err(|_| e("scheme"))?;
        req.set_authority(Some(authority)).map_err(|_| e("authority"))?;
        req.set_path_with_query(Some(if path.is_empty() { "/" } else { path }))
            .map_err(|_| e("path"))?;
        let out = req.body().map_err(|_| e("body"))?;
        if let Some(bytes) = body {
            let stream = out.write().map_err(|_| e("body stream"))?;
            if !write_all(&stream, bytes) {
                return Err(e("writing the body"));
            }
        }
        OutgoingBody::finish(out, None).map_err(|_| e("finishing the body"))?;
        let opts = RequestOptions::new();
        let _ = opts.set_connect_timeout(Some(10_000_000_000));
        // Under the host's 30 s request timeout, so a slow call fails here and the pass goes on.
        let _ = opts.set_first_byte_timeout(Some(25_000_000_000));
        let fut = bindings::wasi::http::outgoing_handler::handle(req, Some(opts))
            .map_err(|err| format!("{err:?}"))?;
        fut.subscribe().block();
        let resp = fut
            .get()
            .ok_or_else(|| e("no response"))?
            .map_err(|_| e("response taken"))?
            .map_err(|err| format!("{err:?}"))?;
        let status = resp.status();
        let headers = resp
            .headers()
            .entries()
            .into_iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), String::from_utf8_lossy(&v).into_owned()))
            .collect();
        let incoming = resp.consume().map_err(|_| e("response body"))?;
        let stream = incoming.stream().map_err(|_| e("response stream"))?;
        let mut buf = Vec::new();
        loop {
            match stream.blocking_read(64 * 1024) {
                Ok(c) if c.is_empty() => break,
                Ok(c) => {
                    buf.extend_from_slice(&c);
                    if buf.len() > MAX_BODY_BYTES {
                        return Err(format!("answered more than {MAX_BODY_BYTES} bytes"));
                    }
                }
                Err(bindings::wasi::io::streams::StreamError::Closed) => break,
                // A failed read is not the end: a truncated reply would parse into a wrong whole one.
                Err(err) => return Err(format!("reading: {err:?}")),
            }
        }
        Ok(Response { status, headers, body: buf })
    }
}

fn respond(out: ResponseOutparam, status: u16, body: &[u8]) {
    let fields = Fields::new();
    let _ = fields.set("content-type", &[b"application/json".to_vec()]);
    let resp = OutgoingResponse::new(fields);
    let _ = resp.set_status_code(status);
    let Ok(b) = resp.body() else { return };
    ResponseOutparam::set(out, Ok(resp));
    if let Ok(stream) = b.write() {
        let _ = write_all(&stream, body);
    }
    let _ = OutgoingBody::finish(b, None);
}

fn method_name(m: &Method) -> String {
    match m {
        Method::Get => "GET".into(),
        Method::Post => "POST".into(),
        Method::Put => "PUT".into(),
        Method::Delete => "DELETE".into(),
        Method::Head => "HEAD".into(),
        Method::Options => "OPTIONS".into(),
        Method::Patch => "PATCH".into(),
        Method::Connect => "CONNECT".into(),
        Method::Trace => "TRACE".into(),
        Method::Other(s) => s.clone(),
    }
}

impl Guest for Component {
    fn handle(request: IncomingRequest, out: ResponseOutparam) {
        let url = request.path_with_query().unwrap_or_default();
        if url.split('?').next() == Some("/health") {
            return respond(out, 200, br#"{"ok":true}"#);
        }
        let cfg = match Config::from_lookup(lookup) {
            Ok(c) => c,
            Err(e) => {
                return respond(
                    out,
                    503,
                    json!({"error": format!("not set up: {e}")}).to_string().as_bytes(),
                )
            }
        };
        let bridge = Bridge::new(cfg, Arc::new(WasiHttp), Box::new(KvStore));
        let method = method_name(&request.method());
        if method == "POST" && url == "/internal/pump" {
            let Some(lease) = take_lease() else {
                return respond(out, 200, br#"{"pumped":false,"why":"a pass is running"}"#);
            };
            // Once, before the first room: the bridge user must exist to create one.
            if bridge.state.read(|d| d.control_room.is_none()) {
                if let Err(e) = bridge.ensure_bridge_user() {
                    eprintln!("matrix-bridge: registering the bridge user: {e}");
                }
            }
            bridge.pump();
            release_lease(lease);
            return respond(out, 200, br#"{"pumped":true}"#);
        }
        let authorization = request
            .headers()
            .get("authorization")
            .first()
            .map(|v| String::from_utf8_lossy(v).into_owned());
        let Ok(body) = read_body(&request) else {
            return respond(out, 413, br#"{"errcode":"M_TOO_LARGE"}"#);
        };
        let (status, v) =
            appservice::handle(&bridge, &method, &url, authorization.as_deref(), &body);
        respond(out, status, v.to_string().as_bytes())
    }
}

bindings::export!(Component with_types_in bindings);
