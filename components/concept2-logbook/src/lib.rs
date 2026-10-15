//! `concept2-logbook` — read a public Concept2 logbook profile over HTTP.
//!
//! Replaces the Python connector `agent-runtime/connectors/concept2.py`. ADR-0095: native only
//! when WASI cannot express it, and this is an HTTPS GET plus string work, so it is a component.
//! Exports `wasi:http/incoming-handler@0.2.0`, imports `wasi:http/outgoing-handler@0.2.0` and
//! nothing else: no key, no state, no secrets.
//!
//!   GET /list?profile=<digits>[&limit=1..30][&page=n]   newest workouts, one per line
//!   GET /workout/<id>?profile=<digits>                  one session: totals and splits
//!
//! The answers are plain text, line for line what the Python printed, so an agent's
//! `http_get` against this component sees what its connector used to.
//!
//! It reaches exactly one host. The authority of every outgoing request is the constant
//! [`logbook::HOST`], the path is built from digits only (`logbook::plan` refuses anything else
//! before a request is made), and a redirect is followed only to a path on that same host.
//!
//! The page-to-text work is `logbook`, pure and unit-tested on the host; the rest is wasm-only.

pub mod logbook;

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
mod guest {
    use crate::bindings::exports::wasi::http::incoming_handler::Guest;
    use crate::bindings::wasi::http::outgoing_handler;
    use crate::bindings::wasi::http::types::{
        Fields, IncomingRequest, Method, OutgoingBody, OutgoingRequest, OutgoingResponse,
        RequestOptions, ResponseOutparam, Scheme,
    };
    use crate::bindings::wasi::io::streams::StreamError;
    use crate::logbook::{self, Plan};

    guestio::guest_write_all!();

    struct Component;

    /// A ceiling on what is held in memory, not a policy: a logbook page is well under a megabyte,
    /// and past this the read gives up rather than growing until the store traps the component.
    const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

    /// Redirects followed before giving up.
    const MAX_REDIRECTS: usize = 3;

    /// One GET of `path` on the logbook host: (status, body, `location` header).
    fn get(path: &str) -> Result<(u16, Vec<u8>, Option<String>), String> {
        let headers = Fields::new();
        let _ = headers.set("user-agent", &[b"Mozilla/5.0 (holon concept2 connector)".to_vec()]);
        let _ = headers.set("accept", &[b"text/html".to_vec()]);
        // No transfer encoding to undo: wasi:http hands the bytes over as they came.
        let _ = headers.set("accept-encoding", &[b"identity".to_vec()]);
        let req = OutgoingRequest::new(headers);
        let e = |m: &str| m.to_string();
        req.set_method(&Method::Get).map_err(|_| e("method"))?;
        req.set_scheme(Some(&Scheme::Https)).map_err(|_| e("scheme"))?;
        req.set_authority(Some(logbook::HOST)).map_err(|_| e("authority"))?;
        req.set_path_with_query(Some(path)).map_err(|_| e("path"))?;
        // A GET has no body, but the request body must still be finished.
        let out = req.body().map_err(|_| e("body"))?;
        OutgoingBody::finish(out, None).map_err(|_| e("finish"))?;
        let opts = RequestOptions::new();
        let _ = opts.set_connect_timeout(Some(20_000_000_000));
        let _ = opts.set_first_byte_timeout(Some(20_000_000_000));
        let _ = opts.set_between_bytes_timeout(Some(20_000_000_000));

        let fut = outgoing_handler::handle(req, Some(opts))
            .map_err(|err| format!("could not start the request: {err:?}"))?;
        fut.subscribe().block();
        let resp = fut
            .get()
            .ok_or_else(|| e("no response"))?
            .map_err(|_| e("response already taken"))?
            .map_err(|err| format!("the request failed: {err:?}"))?;
        let status = resp.status();
        let location = resp
            .headers()
            .get("location")
            .into_iter()
            .next()
            .and_then(|v| String::from_utf8(v).ok());

        let mut buf = Vec::new();
        let incoming = resp.consume().map_err(|_| e("could not read the body"))?;
        let stream = incoming.stream().map_err(|_| e("could not read the body"))?;
        loop {
            match stream.blocking_read(8192) {
                Ok(c) if c.is_empty() => break,
                Ok(c) => {
                    if buf.len() + c.len() > MAX_BODY_BYTES {
                        return Err(e("the page is too large"));
                    }
                    buf.extend_from_slice(&c);
                }
                // `Closed` is how wasi:io says end-of-body; any other error is a read that failed,
                // and a truncated page must not look like a whole one.
                Err(StreamError::Closed) => break,
                Err(_) => return Err(e("the connection failed while reading the page")),
            }
        }
        Ok((status, buf, location))
    }

    /// The page at `path` as text, following redirects that stay on the logbook host.
    fn fetch(path: &str) -> Result<String, String> {
        let mut path = path.to_string();
        for _ in 0..=MAX_REDIRECTS {
            let (status, body, location) = get(&path)?;
            match status {
                200..=299 => return Ok(String::from_utf8_lossy(&body).into_owned()),
                301 | 302 | 303 | 307 | 308 => {
                    path = location.as_deref().and_then(logbook::redirect_target).ok_or_else(
                        || format!("HTTP {status} to somewhere other than the logbook"),
                    )?;
                }
                other => return Err(format!("HTTP {other}")),
            }
        }
        Err("too many redirects".to_string())
    }

    fn respond(response_out: ResponseOutparam, status: u16, body: &str) {
        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"text/plain; charset=utf-8".to_vec()]);
        let response = OutgoingResponse::new(headers);
        let _ = response.set_status_code(status);
        let out = response.body().expect("outgoing body");
        ResponseOutparam::set(response_out, Ok(response));
        if !body.is_empty() {
            let stream = out.write().expect("write stream");
            for chunk in body.as_bytes().chunks(4096) {
                let _ = write_all(&stream, chunk);
            }
        }
        let _ = OutgoingBody::finish(out, None);
    }

    impl Guest for Component {
        fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
            let target = request.path_with_query().unwrap_or_else(|| "/".to_string());
            let (route, query) = target.split_once('?').unwrap_or((&target, ""));
            let is_get = matches!(request.method(), Method::Get);
            let page = match logbook::plan(is_get, route, query) {
                Plan::NotFound => {
                    return respond(
                        response_out,
                        404,
                        "not found: GET /list or GET /workout/<id>\n",
                    )
                }
                Plan::MethodNotAllowed => return respond(response_out, 405, "only GET\n"),
                Plan::Bad(why) => return respond(response_out, 400, &format!("{why}\n")),
                Plan::List { profile, limit, page } => fetch(&logbook::list_path(&profile, page))
                    .map(|html| logbook::listing(&html, limit)),
                Plan::Workout { profile, id } => fetch(&logbook::workout_path(&profile, &id))
                    .map(|html| logbook::workout(&html, &id)),
            };
            match page {
                Ok(text) => respond(response_out, 200, &format!("{text}\n")),
                Err(e) => respond(response_out, 502, &format!("could not read the logbook: {e}\n")),
            }
        }
    }

    crate::bindings::export!(Component with_types_in crate::bindings);
}
