use crate::bindings::exports::wasi::http::incoming_handler::{IncomingRequest, ResponseOutparam};
use crate::bindings::wasi::http::types::{
    Fields, OutgoingBody, OutgoingResponse, RequestOptions,
};
use crate::reply;
use crate::tickets;

pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.as_bytes().to_vec(),
        }
    }

    pub fn err(status: u16, msg: &str) -> Self {
        Self::json(status, &format!("{{\"error\":\"{}\"}}", msg))
    }
}

pub fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
    let method = request.method();
    let path_with_query = request.path_with_query().unwrap_or_else(|| "/".to_string());
    let path = path_with_query.split('?').next().unwrap_or("/");

    let reply = match (method, path) {
        (crate::bindings::wasi::http::types::Method::Get, "/") | (crate::bindings::wasi::http::types::Method::Get, "/index.html") => {
            Reply {
                status: 200,
                content_type: "text/html",
                body: include_bytes!("../ui/index.html").to_vec(),
            }
        }
        (crate::bindings::wasi::http::types::Method::Get, "/styles.css") => Reply {
            status: 200,
            content_type: "text/css",
            body: include_bytes!("../ui/styles.css").to_vec(),
        },
        (crate::bindings::wasi::http::types::Method::Get, "/app.js") => Reply {
            status: 200,
            content_type: "application/javascript",
            body: include_bytes!("../ui/app.js").to_vec(),
        },
        (crate::bindings::wasi::http::types::Method::Get, "/api/tickets") => tickets::list(),
        (crate::bindings::wasi::http::types::Method::Post, "/api/tickets") => {
            let body = read_body(request);
            tickets::create(&body)
        }
        (crate::bindings::wasi::http::types::Method::Post, p) if p.starts_with("/api/tickets/") && p.ends_with("/reply") => {
            let id = p.trim_start_matches("/api/tickets/").trim_end_matches("/reply");
            let body = read_body(request);
            reply::add_reply(id, &body)
        }
        (crate::bindings::wasi::http::types::Method::Post, p) if p.starts_with("/api/tickets/") && p.ends_with("/suggest") => {
            let id = p.trim_start_matches("/api/tickets/").trim_end_matches("/suggest");
            reply::suggest_reply(id)
        }
        (crate::bindings::wasi::http::types::Method::Post, p) if p.starts_with("/api/tickets/") && p.ends_with("/close") => {
            let id = p.trim_start_matches("/api/tickets/").trim_end_matches("/close");
            reply::close_ticket(id)
        }
        _ => Reply::err(404, "not_found"),
    };

    send_reply(response_out, reply);
}

fn read_body(req: IncomingRequest) -> String {
    let incoming_body = req.consume().expect("request body should be readable");
    let stream = incoming_body.stream().expect("stream should be available");
    let mut buf = Vec::new();
    const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
    loop {
        match stream.blocking_read(1024) {
            Ok(bytes) => {
                buf.extend(bytes);
                if buf.len() > MAX_BODY_BYTES {
                    break;
                }
            }
            Err(crate::bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => break,
        }
    }
    String::from_utf8(buf).unwrap_or_default()
}

fn send_reply(response_out: ResponseOutparam, reply: Reply) {
    let headers = Fields::new();
    headers.set(&"content-type".to_string(), &[reply.content_type.as_bytes().to_vec()]).unwrap();

    let response = OutgoingResponse::new(headers);
    response.set_status_code(reply.status).unwrap();

    let outgoing_body = response.body().unwrap();
    let write_stream = outgoing_body.write().unwrap();

    ResponseOutparam::set(response_out, Ok(response));

    if !reply.body.is_empty() {
        for chunk in reply.body.chunks(4096) {
            match write_stream.blocking_write_and_flush(chunk) {
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
    drop(write_stream);
    OutgoingBody::finish(outgoing_body, None).unwrap();
}
