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
        /// Read the whole request body, or fail. Never returns a partial one.
        ///
        /// Expanded by `guestio::guest_read_body!()`.
        fn read_body(
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
