//! `photoquest-domain` — router: auth (register/login/logout/me), the photo
//! routes in `photos.rs`, and the evaluator's callback.
//!
//! The auth half is the same `guestauth`/`guestio` macro sequence every app here
//! uses. The router is NOT `guest_saas_router!`, for one route: the callback
//! `comp-media` POSTs when an evaluation finishes lives under `/internal`, is
//! authenticated by an HMAC over its exact bytes rather than by a bearer, and
//! needs a request header the SaaS router never reads. So it is dispatched here,
//! before anything that expects a login — the same move `events-domain` makes for
//! its SSE stream.
//!
//! Why the bytes of a photo never reach this component at all: ADR-0098.

#[allow(warnings)]
mod bindings;
mod clock;
mod competitions;
mod curation;
mod moderation;
mod photos;
mod progress;
mod quests;
mod rules;

pub use guestauth::Route;

pub const TENANT: &str = "photoquest";
/// The only role register grants. `admin` is honoured when an operator assigns it
/// (`is_admin` reads roles, not this list) but cannot be asked for: an admin reads
/// every photo, and a role is a privilege, not a free-text field.
const ROLES: &[&str] = &["photographer"];

guestio::guest_write_all!();
guestio::guest_bearer!();

/// The callback is the largest body here — 256 sharpness tiles plus Vision's
/// boxes — and is tens of KiB. A megabyte is a ceiling, not a budget.
const MAX_BODY_BYTES: usize = 1024 * 1024;
guestio::guest_read_body_text!(MAX_BODY_BYTES);

guestauth::guest_auth_reply!();
guestauth::guest_introspect!();
guestauth::guest_role_check!(is_admin, "admin");
guestauth::guest_audit!(TENANT);
guestauth::guest_accounts_endpoints!(TENANT, ROLES, "photographer");
guestauth::guest_emit!();

/// One request header's first value, empty when absent.
fn header(request: &bindings::wasi::http::types::IncomingRequest, name: &str) -> String {
    request
        .headers()
        .get(name)
        .first()
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_default()
}

struct Component;

impl bindings::exports::wasi::http::incoming_handler::Guest for Component {
    fn handle(
        request: bindings::wasi::http::types::IncomingRequest,
        response_out: bindings::wasi::http::types::ResponseOutparam,
    ) {
        use bindings::wasi::http::types::Method;
        let path = request.path_with_query().unwrap_or_else(|| "/".into());
        let raw_path = path.split('?').next().unwrap_or("/").to_string();
        let bearer = bearer(&request).unwrap_or_default();
        // Read before the body: `headers()` is not readable once `consume()` has
        // been called.
        let signature = header(&request, "x-media-signature");
        let method = request.method();
        // BYTES, kept as they arrived. The callback's signature is over the raw
        // body, and a body that went through `from_utf8_lossy` first is a
        // different body whenever it was not valid UTF-8.
        let bytes = match method {
            Method::Post | Method::Put | Method::Patch | Method::Delete => {
                read_body_bytes(&request).ok()
            }
            _ => Some(Vec::new()),
        };
        let Some(bytes) = bytes else {
            return emit(response_out, Reply::err(413, "body_too_large"));
        };
        let body = String::from_utf8_lossy(&bytes).into_owned();
        let segments: Vec<String> =
            raw_path.split('/').filter(|s| !s.is_empty()).map(str::to_string).collect();
        let route = Route { segments, bearer };
        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();

        let reply = match (&method, seg.as_slice()) {
            (_, ["health"]) => Reply::json(200, serde_json::json!({"ok": true})),
            // 404 unless config `allow-test-routes = true` (see clock.rs).
            (Method::Post, ["test", "clock"]) => clock::set(&body),
            // Register plus the account book and `bootstrap-admin-email` (moderation.rs).
            (Method::Post, ["register"]) => moderation::register(&body),
            (Method::Post, ["login"]) => login(&body),
            (Method::Post, ["logout"]) => logout(&route),
            (Method::Get, ["me"]) => me(&route),
            // Not behind a login: `comp-media` has no account here. The HMAC is
            // the authentication, checked first thing inside.
            (Method::Post, ["internal", "photos", id, "evaluated"]) => {
                photos::evaluated(id, &bytes, &signature)
            }
            // The game (CONTRACT.md "Game routes"). Most specific first: a photo's
            // reports are moderation's, the rest of /api/photos is photos.rs.
            (_, ["api", "photos", _, "reports"]) | (_, ["api", "admin", ..]) => {
                moderation::handle(&method, &route, &body, &path)
            }
            (_, ["api", "curator", "competitions", ..]) | (_, ["api", "competitions", ..]) => {
                competitions::handle(&method, &route, &body)
            }
            (_, ["api", "curator", ..]) => curation::handle(&method, &route, &body),
            (_, ["api", "journeys", ..])
            | (_, ["api", "quests", ..])
            | (_, ["api", "me", "progress"]) => progress::handle(&method, &route, &body),
            (_, ["api", ..]) => photos::handle(&method, &route, &body),
            _ => Reply::err(404, "not_found"),
        };
        emit(response_out, reply);
    }
}

bindings::export!(Component with_types_in bindings);
