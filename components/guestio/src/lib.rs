//! `guestio` — the `wasi:http` body helpers every component needs, expanded into the caller rather than linked
//!
//! 53 components had written `write_all` and 49 of those were byte-identical. That
//! is not laziness, it is structural: `cargo-component` generates a `bindings.rs`
//! per crate, so `wasi::io::streams::OutputStream` is a DIFFERENT TYPE in every
//! component and no shared function can take one. ADR-0095 names this and concludes
//! that a shared Rust library is not licensed and the control is a lint.
//!
//! This is the third option that neither the ADR nor the lint covers: a macro
//! expands the helper INTO the component, so `crate::bindings::…` resolves at the
//! expansion site and every component still gets its own monomorphic copy — no
//! linked library, no shared types, ADR-0095 intact. What changes is that there is
//! one definition to be right rather than fifty.
//!
//! The lint stays: `reconciler/tests/guestio.rs` checks this file's macro AND any
//! hand-written copy, because a component is still free to write its own and the
//! rule is about the shape, not about where it came from.
//!
//! ```ignore
//! use guestio::guest_write_all;
//! guest_write_all!();     // defines `write_all(&OutputStream, &[u8]) -> bool`
//! ```

#![allow(clippy::crate_in_macro_def)]

/// Define the `write_all` helper: write every byte, waiting when the stream is full.
///
/// The prose here deliberately avoids the literal `fn write_all` + `(` — the lint in
/// `reconciler/tests/guestio.rs` splits a file on that string to find the body it
/// checks, and a mention of it in a doc comment would hand the lint the documentation
/// instead of the code. Which it did, and the lint said so.
///
/// `blocking_write_and_flush` traps above 4096 bytes, so a response larger than that
/// kills the component mid-answer and the caller sees a closed connection with no
/// status. This asks the stream how much it will take (`check_write`), writes that
/// much, and BLOCKS on the pollable when the answer is zero — a loop on a constant
/// would reintroduce the flush-per-4KB this replaced, and one that skipped the
/// `Ok(0)` case would spin instead of waiting.
#[macro_export]
macro_rules! guest_write_all {
    () => {
        /// Write every byte of `bytes`, waiting when the stream is full.
        ///
        /// Expanded by `guestio::guest_write_all!()`.
        fn write_all(
            stream: &crate::bindings::wasi::io::streams::OutputStream,
            mut bytes: &[u8],
        ) -> bool {
            while !bytes.is_empty() {
                let ready = match stream.check_write() {
                    // The stream is full: wait for it rather than spinning on it.
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

/// Define the body-reading helper: every byte, or an error — never a silent prefix.
///
/// As above, the prose avoids the literal name-plus-paren that the lint in
/// `reconciler/tests/guestio.rs` splits files on, so that this comment cannot be
/// handed to the lint in place of the code.
///
/// 76 components wrote this by hand, in 15 distinct implementations. 49 of those
/// were the same function twice over — the top two clusters differed only in a
/// variable name and the wrapping of a comment. The remaining variants disagreed
/// about things that matter: 21 returned a `String` through `from_utf8_lossy`,
/// which silently corrupts any body that is not UTF-8 and cost this repository a
/// round of mangled image uploads, and 6 returned a bare `Vec<u8>` with the errors
/// swallowed.
///
/// Takes the ceiling as an argument rather than reading a `MAX_BODY_BYTES` from the
/// caller's scope. 71 components use 16 MiB and four deliberately use less — 64 KiB,
/// 256 KiB, 1 MiB — so a macro that assumed the common value would have quietly
/// raised four ceilings, which is the one change here nobody would have noticed.
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

/// The body loop, under a name the caller chooses.
///
/// Exists so `guest_read_body_text!` can build on the SAME loop instead of carrying a
/// second copy of it — which is the thing this whole crate is for. Call
/// `guest_read_body!` or `guest_read_body_text!` rather than this.
#[macro_export]
macro_rules! guest_read_body_named {
    ($name:ident, $limit:expr) => {
        /// Read the whole request body, or fail. Never returns a partial one.
        ///
        /// Expanded by `guestio::guest_read_body!()`.
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
                        // A ceiling, not a policy: past this the read stops and the
                        // caller is told, rather than growing until the store's
                        // memory cap traps the component and the connection just
                        // closes with nothing said.
                        if buf.len() + chunk.len() > $limit {
                            return Err(());
                        }
                        buf.extend_from_slice(&chunk);
                    }
                    // `Closed` is how wasi:io says end-of-body; `LastOperationFailed`
                    // is a read that went wrong. Collapsing both into `break` returns
                    // a TRUNCATED body as if it were complete — the same silent
                    // truncation that, on the write side, took four runs to find.
                    Err(crate::bindings::wasi::io::streams::StreamError::Closed) => break,
                    Err(_) => return Err(()),
                }
            }
            Ok(buf)
        }
    };
}

/// Define the bearer-credential helper: find the header, hand the value to `guestfmt`.
///
/// The split is the point. Parsing an `Authorization` value is string work and
/// belongs somewhere it can be unit-tested — `guestfmt::bearer_token` — and 24
/// hand-written copies got the parsing wrong, not the lookup: twenty-two matched a
/// literal `"Bearer "` when RFC 7235 makes the scheme case-insensitive, and sixteen
/// left whitespace attached to the credential.
///
/// What genuinely needs a macro is the two lines around it, because `IncomingRequest`
/// is a different type in every component.
///
/// EVERY value is tried, not just the first. `Fields::get` returns a list, and a
/// request may legally carry more than one `authorization` header; the copies that
/// took `.first()` would refuse a valid credential because something else got there
/// first. Invalid UTF-8 skips that value rather than failing the lookup — a bearer
/// token is ASCII by construction (RFC 6750's `b64token`), so a value that is not
/// UTF-8 is not the credential being offered.
///
/// ```ignore
/// use guestio::guest_bearer;
/// guest_bearer!();     // defines `bearer(&IncomingRequest) -> Option<String>`
/// ```
#[macro_export]
macro_rules! guest_bearer {
    () => {
        /// The bearer credential this request carries, if it carries one.
        ///
        /// Expanded by `guestio::guest_bearer!()`.
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

/// Define a body reader that yields text: the same loop, decoded lossily at the end.
///
/// 21 components returned a `String` from their own copy of this, and the decode is
/// why this is a separate macro rather than a call to `guest_read_body!`:
/// `from_utf8_lossy` replaces every byte that is not valid UTF-8, which is right for
/// a handler about to parse JSON — a mangled body fails the parse either way — and
/// WRONG for anything that stores or forwards what it read. `events-domain` learned
/// that by mangling image uploads.
///
/// So both are defined and the choice is at the call site:
///
///   * `read_body(&request) -> String` — text you are about to parse
///   * `read_body_bytes(&request) -> Result<Vec<u8>, ()>` — anything else
///
/// A failed read yields an EMPTY string, not a partial one. The `Result` is gone by
/// then, so the only honest options were empty or truncated, and a handler parsing an
/// empty body fails cleanly where half a JSON document can parse into something
/// plausible and wrong.
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

        /// The request body as text, with invalid UTF-8 replaced.
        ///
        /// Expanded by `guestio::guest_read_body_text!()`. Use `read_body_bytes` for a
        /// body that is not text.
        fn read_body(request: &crate::bindings::wasi::http::types::IncomingRequest) -> String {
            String::from_utf8_lossy(&read_body_bytes(request).unwrap_or_default()).into_owned()
        }
    };
}

// ---- WASI p3 (`wasi:http@0.3.0-rc-2026-03-15`) --------------------------------
//
// The same helpers for a component whose door is the async `wasi:http/handler`.
// Its bindings come from `wit_bindgen::generate!` inside `mod bindings`, which
// also defines `bindings::p3::http` — an alias, because wit-bindgen suffixes the
// module with its version (`http0_3_0_rc_2026_03_15`) whenever the p2 `wasi:http`
// is in the same resolve, and the root `auth.wit` puts it there for most
// components. Code and these macros name `p3::http`; only the alias moves when
// the WIT version does. There is no `write_all`: a
// p3 `stream<u8>` write has no 4096-byte trap, and `StreamWriter::write_all`
// already loops until every byte is taken.

/// Define `read_body(Request) -> Result<Vec<u8>, ()>` for a p3 component: every
/// byte, or an error — never a silent prefix, never past `$limit`.
///
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

/// The p3 body loop, under a name the caller chooses. Call `guest_p3_read_body!`
/// or `guest_p3_read_body_text!` rather than this.
#[macro_export]
macro_rules! guest_p3_read_body_named {
    ($name:ident, $limit:expr) => {
        /// Read the whole request body, or fail. Never returns a partial one.
        ///
        /// Expanded by `guestio::guest_p3_read_body!()`.
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
                // A ceiling, not a policy — see `guest_read_body_named`.
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
            // The stream ending is not the body arriving whole: in p3 a transfer that
            // failed partway is reported on the trailers future, so a body is only
            // complete once that future says so.
            match trailers.await {
                Ok(_) => Ok(buf),
                Err(_) => Err(()),
            }
        }
    };
}

/// The p3 text reader: `read_body(Request) -> String` (lossy — for text you are
/// about to parse) plus `read_body_bytes(Request) -> Result<Vec<u8>, ()>`.
/// See `guest_read_body_text!` for why both exist.
#[macro_export]
macro_rules! guest_p3_read_body_text {
    ($limit:expr) => {
        $crate::guest_p3_read_body_named!(read_body_bytes, $limit);

        /// The request body as text, with invalid UTF-8 replaced. A failed read
        /// yields an EMPTY string, not a partial one.
        ///
        /// Expanded by `guestio::guest_p3_read_body_text!()`.
        async fn read_body(request: crate::bindings::p3::http::types::Request) -> String {
            String::from_utf8_lossy(&read_body_bytes(request).await.unwrap_or_default())
                .into_owned()
        }
    };
}

/// Define `bearer(&Request) -> Option<String>` for a p3 component. Same rules as
/// `guest_bearer!`: every `authorization` value is tried, parsing is `guestfmt`'s.
#[macro_export]
macro_rules! guest_p3_bearer {
    () => {
        /// The bearer credential this request carries, if it carries one.
        ///
        /// Expanded by `guestio::guest_p3_bearer!()`.
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

/// Define the p3 response builders:
///
///   * `respond_with(status, Fields, body) -> Result<Response, ErrorCode>`
///   * `respond(status, content_type, body)` — the common one-header case
///
/// An empty body gets no stream, and the trailers writer is dropped rather than
/// written: its default (`Ok(None)`) is what the host reads, with no handshake.
#[macro_export]
macro_rules! guest_p3_respond {
    () => {
        /// A response with these headers and this whole body.
        ///
        /// Expanded by `guestio::guest_p3_respond!()`.
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

        /// A response with one `content-type` header and this whole body.
        ///
        /// Expanded by `guestio::guest_p3_respond!()`.
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

/// Define `fetch(method, url, headers, body) -> Result<(u16, Vec<u8>), String>`:
/// one outbound request through the p3 `wasi:http/client`, the response body read
/// whole under `$limit` (same rules as `guest_p3_read_body!` — a failed transfer is
/// an error, never a short body). `url` is `scheme://authority/path?query`. The
/// host's egress allow-list applies exactly as it does to p2's outgoing-handler.
///
/// ```ignore
/// guestio::guest_p3_fetch!(16 * 1024 * 1024);
/// let (status, body) = fetch(Method::Post, "https://api.example.com/v1/x",
///                            &[("content-type", "application/json")], bytes).await?;
/// ```
#[macro_export]
macro_rules! guest_p3_fetch {
    ($limit:expr) => {
        /// One outbound request; the whole response body, or an error.
        ///
        /// Expanded by `guestio::guest_p3_fetch!()`.
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
