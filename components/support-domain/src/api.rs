use crate::bindings::p3::http::types::{ErrorCode, Request, Response};
use crate::reply;
use crate::tickets;

pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(status: u16, body: &str) -> Self {
        Self { status, content_type: "application/json", body: body.as_bytes().to_vec() }
    }

    pub fn err(status: u16, msg: &str) -> Self {
        Self::json(status, &format!("{{\"error\":\"{}\"}}", msg))
    }
}

pub async fn handle(request: Request) -> Result<Response, ErrorCode> {
    let method = request.get_method();
    let path_with_query = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
    let path = path_with_query.split('?').next().unwrap_or("/");

    let reply = match (method, path) {
        (crate::bindings::p3::http::types::Method::Get, "/")
        | (crate::bindings::p3::http::types::Method::Get, "/index.html") => Reply {
            status: 200,
            content_type: "text/html",
            body: include_bytes!("../ui/index.html").to_vec(),
        },
        (crate::bindings::p3::http::types::Method::Get, "/styles.css") => Reply {
            status: 200,
            content_type: "text/css",
            body: include_bytes!("../ui/styles.css").to_vec(),
        },
        (crate::bindings::p3::http::types::Method::Get, "/app.js") => Reply {
            status: 200,
            content_type: "application/javascript",
            body: include_bytes!("../ui/app.js").to_vec(),
        },
        (crate::bindings::p3::http::types::Method::Get, "/api/tickets") => tickets::list(),
        (crate::bindings::p3::http::types::Method::Post, "/api/tickets") => {
            let body = read_body(request).await;
            tickets::create(&body)
        }
        (crate::bindings::p3::http::types::Method::Post, p)
            if p.starts_with("/api/tickets/") && p.ends_with("/reply") =>
        {
            let id = p.trim_start_matches("/api/tickets/").trim_end_matches("/reply");
            let body = read_body(request).await;
            reply::add_reply(id, &body)
        }
        (crate::bindings::p3::http::types::Method::Post, p)
            if p.starts_with("/api/tickets/") && p.ends_with("/suggest") =>
        {
            let id = p.trim_start_matches("/api/tickets/").trim_end_matches("/suggest");
            reply::suggest_reply(id)
        }
        (crate::bindings::p3::http::types::Method::Post, p)
            if p.starts_with("/api/tickets/") && p.ends_with("/close") =>
        {
            let id = p.trim_start_matches("/api/tickets/").trim_end_matches("/close");
            reply::close_ticket(id)
        }
        _ => Reply::err(404, "not_found"),
    };

    respond(reply.status, reply.content_type, reply.body)
}

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
guestio::guest_p3_read_body_named!(read_body_bytes, MAX_BODY_BYTES);

/// The body as UTF-8 — empty if it is unreadable, over the ceiling, or not UTF-8.
async fn read_body(req: Request) -> String {
    String::from_utf8(read_body_bytes(req).await.unwrap_or_default()).unwrap_or_default()
}
guestio::guest_p3_respond!();
