#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../slug/wit",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../id-generate/wit",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../cache/wit",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../rate-limiter/wit",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "wit",
        ],
        world: "shortlink:app/shortlink-app",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::cache::store::cache;
use bindings::id::generate::generator as ids;
use bindings::ratelimit::guard::limiter;
use bindings::ratelimit::guard::limiter::LimitError;
use bindings::records::store::store as records;
use bindings::slug::generate::generator as slug;
use bindings::wasi::keyvalue::atomics;
use bindings::wasi::keyvalue::store as kv;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};

struct Component;

const LINKS: &str = "links";
const CODE_LEN: u8 = 7;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let query = path.split_once('?').map(|x| x.1).unwrap_or("").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => Outcome::Json(
                200,
                json!({
                    "service": "shortlink",
                    "create": "POST /api/links {url, slug?, title?}",
                    "list": "GET /api/links",
                    "stats": "GET /api/links/{id}",
                    "redirect": "GET /{code}"
                })
                .to_string(),
            ),
            (Method::Post, ["api", "links"]) => create_link(request).await,
            (Method::Get, ["api", "links"]) => list_links(&query),
            (Method::Get, ["api", "links", id]) => get_link(id),
            (Method::Delete, ["api", "links", id]) => delete_link(id),
            (Method::Get, [code]) => redirect(code),
            _ => Outcome::NotFound,
        };
        emit(result)
    }
}

enum Outcome {
    Json(u16, String),
    Redirect(String),
    Limited(u32),
    Bad(String),
    Err(u16, String),
    NotFound,
}

#[derive(Deserialize)]
struct CreateReq {
    url: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

async fn create_link(request: Request) -> Outcome {
    // No auth, so the client IP is the rate-limit identity.
    let client = format!(
        "shortlink:create:{}",
        header(&request, "x-forwarded-for").unwrap_or_else(|| "anon".to_string())
    );
    match limiter::check(&client) {
        Ok(_) => {}
        Err(LimitError::Locked(secs)) => return Outcome::Limited(secs),
        Err(LimitError::BackendUnavailable(m)) => return Outcome::Err(503, m),
    }

    let req: CreateReq =
        match read_body(request).await.and_then(|b| serde_json::from_slice(&b).map_err(|_| ())) {
            Ok(r) => r,
            Err(_) => return Outcome::Bad("expected json body {url, slug?, title?}".into()),
        };
    if !(req.url.starts_with("http://") || req.url.starts_with("https://")) || req.url.len() > 2048
    {
        return Outcome::Bad("url must be http(s) and under 2048 chars".into());
    }

    let code = match mint_code(req.slug.as_deref()) {
        Ok(c) => c,
        Err(o) => return o,
    };

    let data = json!({
        "code": code,
        "url": req.url,
        "title": req.title.unwrap_or_default(),
    });
    let entry = match records::create(LINKS, &data.to_string(), &["code".to_string()]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };

    // Best-effort: the redirect path rebuilds the cache on a miss.
    let _ = cache::set(&code, req.url.as_bytes(), 0);
    // The limiter only counts failures, so every create spends one.
    let _ = limiter::record_failure(&client);

    Outcome::Json(201, link_json(&entry, None).to_string())
}

fn mint_code(alias: Option<&str>) -> Result<String, Outcome> {
    match alias {
        Some(raw) => {
            let desired = slug::slugify(raw);
            if desired.is_empty() {
                return Err(Outcome::Bad("slug reduces to empty".into()));
            }
            let mut taken = Vec::new();
            for _ in 0..5 {
                let candidate = if taken.is_empty() {
                    desired.clone()
                } else {
                    slug::uniquify(&desired, &taken)
                };
                if candidate.len() > 64 || is_reserved(&candidate) {
                    return Err(Outcome::Bad("slug unavailable".into()));
                }
                if !code_taken(&candidate)? {
                    return Ok(candidate);
                }
                taken.push(candidate);
            }
            Err(Outcome::Err(409, "slug and its suffixes are taken".into()))
        }
        None => {
            // ponytail: 5 tries at 7 url-safe chars; collisions are ~never.
            for _ in 0..5 {
                let candidate = ids::nanoid(CODE_LEN);
                if !code_taken(&candidate)? {
                    return Ok(candidate);
                }
            }
            Err(Outcome::Err(503, "could not mint a free code".into()))
        }
    }
}

fn is_reserved(code: &str) -> bool {
    code == "api"
}

fn code_taken(code: &str) -> Result<bool, Outcome> {
    match records::find_by(LINKS, "code", &json!(code).to_string()) {
        Ok(hits) => Ok(!hits.is_empty()),
        Err(e) => Err(store_err(e)),
    }
}

fn redirect(code: &str) -> Outcome {
    if let Ok(Some(bytes)) = cache::get(code) {
        if let Ok(url) = String::from_utf8(bytes) {
            bump_clicks(code);
            return Outcome::Redirect(url);
        }
    }
    let hits = match records::find_by(LINKS, "code", &json!(code).to_string()) {
        Ok(h) => h,
        Err(e) => return store_err(e),
    };
    let Some(entry) = hits.first() else {
        return Outcome::NotFound;
    };
    let url = match field(&entry.data, "url") {
        Some(u) => u,
        None => return Outcome::Err(500, "corrupt link record".into()),
    };
    let _ = cache::set(code, url.as_bytes(), 0);
    bump_clicks(code);
    Outcome::Redirect(url)
}

/// Fire-and-forget: a lost bump beats a slow redirect.
fn bump_clicks(code: &str) {
    if let Ok(bucket) = kv::open("shortlink") {
        let _ = atomics::increment(&bucket, &format!("clicks:{code}"), 1);
    }
}

fn clicks(code: &str) -> u64 {
    // increment-by-0 reads the counter without a second read API.
    kv::open("shortlink")
        .and_then(|b| atomics::increment(&b, &format!("clicks:{code}"), 0))
        .unwrap_or(0)
}

fn get_link(id: &str) -> Outcome {
    match records::get(LINKS, id) {
        Ok(entry) => {
            let n = field(&entry.data, "code").map(|c| clicks(&c)).unwrap_or(0);
            Outcome::Json(200, link_json(&entry, Some(n)).to_string())
        }
        Err(records::StoreError::NotFound) => Outcome::NotFound,
        Err(e) => store_err(e),
    }
}

fn delete_link(id: &str) -> Outcome {
    let entry = match records::get(LINKS, id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Outcome::NotFound,
        Err(e) => return store_err(e),
    };
    if let Err(e) = records::delete(LINKS, id) {
        return store_err(e);
    }
    if let Some(code) = field(&entry.data, "code") {
        let _ = cache::invalidate(&code);
    }
    Outcome::Json(200, "{\"deleted\":true}".into())
}

fn list_links(query: &str) -> Outcome {
    let limit = query_param(query, "limit").and_then(|s| s.parse().ok()).unwrap_or(0);
    let after = query_param(query, "after").unwrap_or_default();
    match records::list_records(LINKS, limit, &after) {
        Ok(page) => {
            let links: Vec<Value> = page.entries.iter().map(|e| link_json(e, None)).collect();
            Outcome::Json(200, json!({ "links": links, "next": page.next }).to_string())
        }
        Err(e) => store_err(e),
    }
}

fn link_json(entry: &records::Entry, clicks: Option<u64>) -> Value {
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let mut v = json!({
        "id": entry.id,
        "code": data["code"],
        "url": data["url"],
        "title": data["title"],
        "created": entry.created,
    });
    if let Some(n) = clicks {
        v["clicks"] = json!(n);
    }
    v
}

fn field(data: &str, name: &str) -> Option<String> {
    serde_json::from_str::<Value>(data).ok()?.get(name)?.as_str().map(str::to_string)
}

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::NotFound,
        records::StoreError::InvalidJson(m) => Outcome::Bad(m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

/// Backstop below wasmtime's 64 MiB per-store cap, past which the guest traps.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body!(MAX_BODY_BYTES);
guestio::guest_p3_respond!();

fn header(request: &Request, name: &str) -> Option<String> {
    request.get_headers().get(name).into_iter().find_map(|v| String::from_utf8(v).ok())
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let mut it = kv.splitn(2, '=');
        (it.next()? == key).then(|| it.next().unwrap_or("").to_string())
    })
}

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
    match result {
        Outcome::Json(code, body) => reply(code, &[], body.as_bytes()),
        Outcome::Redirect(url) => reply(302, &[("location", &url)], b""),
        Outcome::Limited(secs) => reply(
            429,
            &[("retry-after", &secs.to_string())],
            format!("{{\"error\":\"rate_limited\",\"retryAfter\":{secs}}}").as_bytes(),
        ),
        Outcome::Bad(msg) => reply(400, &[], json!({ "error": msg }).to_string().as_bytes()),
        Outcome::Err(code, msg) => reply(code, &[], json!({ "error": msg }).to_string().as_bytes()),
        Outcome::NotFound => reply(404, &[], b"{\"error\":\"not_found\"}"),
    }
}

fn reply(status: u16, extra: &[(&str, &str)], body: &[u8]) -> Result<Response, ErrorCode> {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    for (k, v) in extra {
        let _ = headers.set(k.as_ref(), &[v.as_bytes().to_vec()]);
    }
    respond_with(status, headers, body.to_vec())
}

bindings::export!(Component with_types_in bindings);
