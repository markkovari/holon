#[allow(warnings)]
mod bindings;
mod handlers;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::{json, Value};

guestio::guest_write_all!();
guestio::guest_bearer!();
guestio::guest_read_body_text!(16 * 1024 * 1024);

pub struct Reply {
    pub status: u16,
    pub json: Value,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Reply { status, json: body }
    }
    pub fn err(status: u16, code: &str) -> Self {
        Reply::json(status, json!({ "error": code }))
    }
    pub fn no_content() -> Self {
        Reply::json(204, Value::Null)
    }
}

pub struct Route {
    pub segments: Vec<String>,
    pub query: String,
    pub bearer: String,
    pub idempotency_key: String,
}

use guestfmt::percent_decode as percent;

fn header(request: &IncomingRequest, name: &str) -> String {
    let fields = request.headers();
    let values = fields.get(name);
    values.first().map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default()
}

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".into());
        let (raw_path, query) = match path.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let bearer_val = bearer(&request).unwrap_or_default();
        let route = Route {
            segments: raw_path.split('/').filter(|s| !s.is_empty()).map(percent).collect(),
            query,
            bearer: bearer_val,
            idempotency_key: header(&request, "idempotency-key"),
        };
        let method = request.method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(&request),
            _ => String::new(),
        };

        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
        
        // Static UI Serving
        if seg.is_empty() || seg.as_slice() == ["index.html"] {
            return serve_static(response_out, include_str!("../ui/index.html"), "text/html");
        }
        if seg.as_slice() == ["style.css"] {
            return serve_static(response_out, include_str!("../ui/style.css"), "text/css");
        }
        if seg.as_slice() == ["app.js"] {
            return serve_static(response_out, include_str!("../ui/app.js"), "application/javascript");
        }

        let Reply { status, json: payload } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            ["register"] => handlers::register(&method, &body),
            ["login"] => handlers::login(&method, &body),
            ["api", "orders"] => handlers::orders(&method, &route, &body),
            ["api", "orders", id, "fulfill"] => handlers::fulfill_order(&method, &route, id, &body),
            _ => Reply::err(404, &format!("not_found: {:?}", seg)),
        };

        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(status);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            if !payload.is_null() {
                let _ = write_all(&stream, payload.to_string().as_bytes());
            }
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

fn serve_static(response_out: ResponseOutparam, content: &str, content_type: &str) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
    let resp = OutgoingResponse::new(headers);
    let _ = resp.set_status_code(200);
    let out = resp.body().expect("body");
    ResponseOutparam::set(response_out, Ok(resp));
    if let Ok(stream) = out.write() {
        let _ = write_all(&stream, content.as_bytes());
        drop(stream);
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
