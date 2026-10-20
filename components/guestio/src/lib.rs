//! `guestio` — the `wasi:http` body helpers every component needs, expanded into the caller rather than linked.
//!
//! ```ignore
//! use guestio::guest_write_all;
//! guest_write_all!();     // defines `write_all(&OutputStream, &[u8]) -> bool`
//! ```

#![allow(clippy::crate_in_macro_def)]

/// `blocking_write_and_flush` traps above 4096 bytes: write what `check_write` allows and block
/// on the pollable at zero (a fixed-size loop flushes per 4KB; skipping `Ok(0)` spins).
#[macro_export]
macro_rules! guest_write_all {
    () => {
        fn write_all(
            stream: &crate::bindings::wasi::io::streams::OutputStream,
            mut bytes: &[u8],
        ) -> bool {
            while !bytes.is_empty() {
                let ready = match stream.check_write() {
                    Ok(0) => {
                        stream.subscribe().block();
                        continue;
                    }
                    Ok(n) => n as usize,
                    Err(_) => return false,
                };
                let take = ready.min(bytes.len());
                if stream.write(&bytes[..take]).is_err() {
                    return false;
                }
                bytes = &bytes[take..];
            }
            stream.blocking_flush().is_ok()
        }
    };
}

/// The ceiling is an argument: a few callers deliberately use less than the usual 16 MiB.
///
/// ```ignore
/// use guestio::guest_read_body;
/// const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// guest_read_body!(MAX_BODY_BYTES);
/// ```
#[macro_export]
macro_rules! guest_read_body {
    ($limit:expr) => {
        $crate::guest_read_body_named!(read_body, $limit);
    };
}

/// Shared by `guest_read_body!` and `guest_read_body_text!`; call those instead.
#[macro_export]
macro_rules! guest_read_body_named {
    ($name:ident, $limit:expr) => {
        fn $name(
            request: &crate::bindings::wasi::http::types::IncomingRequest,
        ) -> Result<Vec<u8>, ()> {
            let body = request.consume().map_err(|_| ())?;
            let stream = body.stream().map_err(|_| ())?;
            let mut buf = Vec::new();
            loop {
                match stream.blocking_read(8192) {
                    Ok(chunk) if chunk.is_empty() => break,
                    Ok(chunk) => {
                        // Past the ceiling the caller is told, rather than growing until the memory cap traps.
                        if buf.len() + chunk.len() > $limit {
                            return Err(());
                        }
                        buf.extend_from_slice(&chunk);
                    }
                    // `Closed` is end-of-body, `LastOperationFailed` a failed read; collapsing both truncates silently.
                    Err(crate::bindings::wasi::io::streams::StreamError::Closed) => break,
                    Err(_) => return Err(()),
                }
            }
            Ok(buf)
        }
    };
}

/// Every `authorization` value is tried (several are legal); the parsing is `guestfmt::bearer_token`'s.
///
///
/// ```ignore
/// use guestio::guest_bearer;
/// guest_bearer!();     // defines `bearer(&IncomingRequest) -> Option<String>`
/// ```
#[macro_export]
macro_rules! guest_bearer {
    () => {
        fn bearer(request: &crate::bindings::wasi::http::types::IncomingRequest) -> Option<String> {
            request
                .headers()
                .get(&"authorization".to_string())
                .into_iter()
                .filter_map(|v| String::from_utf8(v).ok())
                .find_map(|v| guestfmt::bearer_token(&v).map(str::to_string))
        }
    };
}

/// Lossy text for a body about to be parsed; `read_body_bytes` for anything stored or forwarded.
/// A failed read yields an empty string: half a JSON document can parse into something wrong.
///
/// ```ignore
/// use guestio::guest_read_body_text;
/// const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// guest_read_body_text!(MAX_BODY_BYTES);
/// ```
#[macro_export]
macro_rules! guest_read_body_text {
    ($limit:expr) => {
        $crate::guest_read_body_named!(read_body_bytes, $limit);

        fn read_body(request: &crate::bindings::wasi::http::types::IncomingRequest) -> String {
            String::from_utf8_lossy(&read_body_bytes(request).unwrap_or_default()).into_owned()
        }
    };
}

// `bindings::p3::http` aliases the module wit-bindgen version-suffixes when p2 `wasi:http` is in
// the same resolve. No p3 `write_all`: a p3 `stream<u8>` write has no 4096-byte trap.

/// ```ignore
/// const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// guestio::guest_p3_read_body!(MAX_BODY_BYTES);
/// ```
#[macro_export]
macro_rules! guest_p3_read_body {
    ($limit:expr) => {
        $crate::guest_p3_read_body_named!(read_body, $limit);
    };
}

/// Shared by `guest_p3_read_body!` and `guest_p3_read_body_text!`; call those instead.
#[macro_export]
macro_rules! guest_p3_read_body_named {
    ($name:ident, $limit:expr) => {
        async fn $name(request: crate::bindings::p3::http::types::Request) -> Result<Vec<u8>, ()> {
            let (res_tx, res_rx) = crate::bindings::wit_future::new(|| Ok(()));
            let (mut body, trailers) =
                crate::bindings::p3::http::types::Request::consume_body(request, res_rx);
            let mut buf: Vec<u8> = Vec::new();
            loop {
                // `read` fills spare capacity; at most 8 KiB past the ceiling.
                buf.reserve(8192);
                let (status, b) = body.read(buf).await;
                buf = b;
                if buf.len() > $limit {
                    return Err(());
                }
                match status {
                    wit_bindgen::StreamResult::Complete(_) => {}
                    wit_bindgen::StreamResult::Dropped => break,
                    wit_bindgen::StreamResult::Cancelled => return Err(()),
                }
            }
            drop(res_tx);
            // In p3 a failed transfer is reported on the trailers future, not by the stream ending.
            match trailers.await {
                Ok(_) => Ok(buf),
                Err(_) => Err(()),
            }
        }
    };
}

#[macro_export]
macro_rules! guest_p3_read_body_text {
    ($limit:expr) => {
        $crate::guest_p3_read_body_named!(read_body_bytes, $limit);

        async fn read_body(request: crate::bindings::p3::http::types::Request) -> String {
            String::from_utf8_lossy(&read_body_bytes(request).await.unwrap_or_default())
                .into_owned()
        }
    };
}

#[macro_export]
macro_rules! guest_p3_bearer {
    () => {
        fn bearer(request: &crate::bindings::p3::http::types::Request) -> Option<String> {
            request
                .get_headers()
                .get("authorization")
                .into_iter()
                .filter_map(|v| String::from_utf8(v).ok())
                .find_map(|v| guestfmt::bearer_token(&v).map(str::to_string))
        }
    };
}

/// An empty body gets no stream; the trailers writer is dropped, not written (the host reads the default).
#[macro_export]
macro_rules! guest_p3_respond {
    () => {
        fn respond_with(
            status: u16,
            headers: crate::bindings::p3::http::types::Fields,
            body: Vec<u8>,
        ) -> Result<
            crate::bindings::p3::http::types::Response,
            crate::bindings::p3::http::types::ErrorCode,
        > {
            let (trailers_tx, trailers_rx) = crate::bindings::wit_future::new(|| Ok(None));
            drop(trailers_tx);
            let contents = if body.is_empty() {
                None
            } else {
                let (mut tx, rx) = crate::bindings::wit_stream::new();
                wit_bindgen::spawn_local(async move {
                    tx.write_all(body).await;
                });
                Some(rx)
            };
            let (response, _sent) =
                crate::bindings::p3::http::types::Response::new(headers, contents, trailers_rx);
            response.set_status_code(status).map_err(|()| {
                crate::bindings::p3::http::types::ErrorCode::InternalError(Some(format!(
                    "bad status {status}"
                )))
            })?;
            Ok(response)
        }

        #[allow(dead_code)]
        fn respond(
            status: u16,
            content_type: &str,
            body: impl Into<Vec<u8>>,
        ) -> Result<
            crate::bindings::p3::http::types::Response,
            crate::bindings::p3::http::types::ErrorCode,
        > {
            let headers = crate::bindings::p3::http::types::Fields::new();
            let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
            respond_with(status, headers, body.into())
        }
    };
}

/// The host's egress allow-list applies as it does to p2's outgoing-handler.
///
/// ```ignore
/// guestio::guest_p3_fetch!(16 * 1024 * 1024);
/// let (status, body) = fetch(Method::Post, "https://api.example.com/v1/x",
///                            &[("content-type", "application/json")], bytes).await?;
/// ```
#[macro_export]
macro_rules! guest_p3_fetch {
    ($limit:expr) => {
        #[allow(dead_code)]
        async fn fetch(
            method: crate::bindings::p3::http::types::Method,
            url: &str,
            headers: &[(&str, &str)],
            body: Vec<u8>,
        ) -> Result<(u16, Vec<u8>), String> {
            use crate::bindings::p3::http::types::{Fields, Request, Response, Scheme};
            let (scheme, rest) = match url.split_once("://") {
                Some(("https", r)) => (Scheme::Https, r),
                Some(("http", r)) => (Scheme::Http, r),
                _ => return Err(format!("unsupported url {url:?}")),
            };
            let (authority, path) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, "/"),
            };
            let fields = Fields::new();
            for (k, v) in headers {
                fields.append(k, v.as_bytes()).map_err(|e| format!("header {k}: {e:?}"))?;
            }
            let (trailers_tx, trailers_rx) = crate::bindings::wit_future::new(|| Ok(None));
            drop(trailers_tx);
            let contents = if body.is_empty() {
                None
            } else {
                let (mut tx, rx) = crate::bindings::wit_stream::new();
                wit_bindgen::spawn_local(async move {
                    tx.write_all(body).await;
                });
                Some(rx)
            };
            let (req, _sent) = Request::new(fields, contents, trailers_rx, None);
            req.set_method(&method).map_err(|()| "bad method".to_string())?;
            req.set_scheme(Some(&scheme)).map_err(|()| "bad scheme".to_string())?;
            req.set_authority(Some(authority)).map_err(|()| "bad authority".to_string())?;
            req.set_path_with_query(Some(path)).map_err(|()| "bad path".to_string())?;
            let resp = crate::bindings::p3::http::client::send(req)
                .await
                .map_err(|e| format!("send: {e:?}"))?;
            let status = resp.get_status_code();
            let (res_tx, res_rx) = crate::bindings::wit_future::new(|| Ok(()));
            let (mut stream, trailers) = Response::consume_body(resp, res_rx);
            let mut buf: Vec<u8> = Vec::new();
            loop {
                buf.reserve(8192);
                let (st, b) = stream.read(buf).await;
                buf = b;
                if buf.len() > $limit {
                    return Err("response body over the limit".into());
                }
                match st {
                    wit_bindgen::StreamResult::Complete(_) => {}
                    wit_bindgen::StreamResult::Dropped => break,
                    wit_bindgen::StreamResult::Cancelled => {
                        return Err("body read cancelled".into())
                    }
                }
            }
            drop(res_tx);
            trailers.await.map_err(|e| format!("response body: {e:?}"))?;
            Ok((status, buf))
        }
    };
}
