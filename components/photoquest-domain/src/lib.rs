//! Not `guest_saas_router!`: comp-media's /internal callback is HMAC-authenticated over its exact bytes,
//! not a bearer, so it is dispatched before anything expecting a login. Photo bytes never reach here (ADR-0098).

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../audit-log/wit",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/ratelimit-guard",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../policy-guard/wit",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../media-pipeline/wit",
            "../webhook-sign/wit",
            "wit",
        ],
        world: "photoquest:domain/photoquest-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
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
/// `admin` is honoured when an operator assigns it (`is_admin` reads roles), but cannot be asked for.
const ROLES: &[&str] = &["photographer"];

guestio::guest_p3_bearer!();
guestio::guest_p3_respond!();

/// The callback is the largest body (tens of KiB); a megabyte is a ceiling, not a budget.
const MAX_BODY_BYTES: usize = 1024 * 1024;
guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

guestauth::guest_auth_reply!();
guestauth::guest_introspect!();
guestauth::guest_role_check!(is_admin, "admin");
guestauth::guest_p3_audit!(TENANT);
guestauth::guest_accounts_endpoints!(TENANT, ROLES, "photographer");
guestauth::guest_p3_emit!();

fn header(request: &bindings::p3::http::types::Request, name: &str) -> String {
    request
        .get_headers()
        .get(name)
        .first()
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_default()
}

struct Component;

impl bindings::p3::handler::Guest for Component {
    async fn handle(
        request: bindings::p3::http::types::Request,
    ) -> Result<bindings::p3::http::types::Response, bindings::p3::http::types::ErrorCode> {
        use bindings::p3::http::types::Method;
        let path = request.get_path_with_query().unwrap_or_else(|| "/".into());
        let raw_path = path.split('?').next().unwrap_or("/").to_string();
        let bearer = bearer(&request).unwrap_or_default();
        // Read before the body: reading it consumes the request.
        let signature = header(&request, "x-media-signature");
        let method = request.get_method();
        // BYTES, kept as they arrived: the callback's signature is over the raw body, and `from_utf8_lossy` first would change it.
        let bytes = match method {
            Method::Post | Method::Put | Method::Patch | Method::Delete => {
                read_body_bytes(request).await.ok()
            }
            _ => Some(Vec::new()),
        };
        let Some(bytes) = bytes else {
            return emit(Reply::err(413, "body_too_large"));
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
            (Method::Post, ["register"]) => moderation::register(&body),
            (Method::Post, ["login"]) => login(&body),
            (Method::Post, ["logout"]) => logout(&route),
            (Method::Get, ["me"]) => me(&route),
            // Not behind a login: `comp-media` has no account here. The HMAC is the authentication, checked first thing inside.
            (Method::Post, ["internal", "photos", id, "evaluated"]) => {
                photos::evaluated(id, &bytes, &signature)
            }
            // Most specific first: a photo's reports are moderation's, the rest of /api/photos is photos.rs.
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
        emit(reply)
    }
}

bindings::export!(Component with_types_in bindings);
