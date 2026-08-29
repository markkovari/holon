//! Nobody writes an unbounded payload in one call again.
//!
//! `wasi:io`'s `blocking-write-and-flush` accepts at most 4096 bytes and TRAPS
//! above that instead of returning an error. A component that hands it a whole
//! response body dies mid-write, and what the caller sees is a closed connection:
//! `hyper::Error(IncompleteMessage)` at the host, `connection closed before
//! message completed` at the ingress, and — three layers up — a JSON parse error
//! about an empty string. Nothing in that chain mentions a size or a write.
//!
//! It cost four failed starts of a real run to find, and it was in the repository
//! from the beginning: 30 of 91 write sites, including the router of the app the
//! run was building. Small payloads never hit it, so it waited for a contract file
//! to grow from 3645 bytes to 4573.
//!
//! Two shapes are accepted, and a third is tolerated by name:
//!
//!   * `write_all(&stream, bytes)` — the helper, which asks `check-write` how much
//!     the stream will take and flushes once. This is what new code should use.
//!   * a `.chunks(4096)` loop — older, correct, more flushes than it needs.
//!   * anything in `ALLOWED`, for a payload that is a short literal and cannot grow.
//!
//! This is a lint, not a law: if a write is provably small, add it to `ALLOWED`
//! with the reason. What must not happen is a new unbounded write appearing by
//! accident, which is exactly how the last one arrived.

use std::path::PathBuf;

use comp_reconciler::fleet::repo_root;

/// Write sites that may stay as they are, with why. `<crate>/<file>:<payload>`.
///
/// Empty today. It exists so that a genuinely bounded write — a fixed protocol
/// preamble, say — has somewhere to go other than a workaround.
const ALLOWED: &[(&str, &str)] = &[];

/// Read loops that may keep treating a failed read as the end of the body.
const READS_ALLOWED: &[(&str, &str)] = &[
    (
        "agent-probe",
        "reads its own test input; a truncated read fails an assertion rather than \
         silently storing half a payload",
    ),
    (
        "eshop-gateway",
        "proxies to another component, which sees the truncation as a malformed \
         request and refuses it",
    ),
    ("photo-critic", "a truncated image fails to decode; the failure is loud and immediate"),
    (
        "bench-suite",
        "counts bytes to measure throughput; a failed read shows up as a number so \
         far off that nobody reads it as a result",
    ),
];

fn guest_sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(repo_root().join("components")) else { return out };
    for dir in dirs.flatten() {
        let src = dir.path().join("src");
        let Ok(files) = std::fs::read_dir(&src) else { continue };
        for f in files.flatten() {
            let p = f.path();
            // Generated, enormous, and not ours to lint.
            if p.extension().is_some_and(|e| e == "rs")
                && p.file_name().is_some_and(|n| n != "bindings.rs")
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn no_component_writes_an_unbounded_payload_in_one_call() {
    let mut offenders = Vec::new();
    for path in guest_sources() {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.contains("blocking_write_and_flush") {
                continue;
            }
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") {
                continue;
            }
            // A chunking loop can be a few lines above the call itself, so the
            // window looks back rather than at the line alone.
            let from = i.saturating_sub(4);
            if lines[from..=i].iter().any(|l| l.contains("chunks(")) {
                continue;
            }
            let rel = path
                .strip_prefix(repo_root().join("components"))
                .unwrap_or(&path)
                .display()
                .to_string();
            if ALLOWED.iter().any(|(site, _)| rel.starts_with(site)) {
                continue;
            }
            offenders.push(format!("  {rel}:{} — {}", i + 1, trimmed.trim_end()));
        }
    }
    assert!(
        offenders.is_empty(),
        "these write a payload in one call, which traps above 4096 bytes and kills \
         the component mid-response:\n{}\n\nUse the `write_all` helper (see any \
         component that has one), or add the site to ALLOWED in this test with the \
         reason it cannot grow.",
        offenders.join("\n")
    );
}

/// A failed read is not the end of the body.
///
/// `wasi:io`'s `blocking-read` signals end-of-body with `Err(StreamError::Closed)`
/// and a genuine failure with `Err(StreamError::LastOperationFailed)`. Collapsing
/// both into `break` — which 53 of 55 `read_body` copies did — returns a TRUNCATED
/// body as if it were complete. For a handler that parses, that is a confusing
/// 400; for `upload-drop` or `artifact-probe`, which store what they read, it is a
/// half a file accepted as whole.
///
/// It is the same silent truncation as the write side, in the other direction, and
/// it was found by asking the question that fixed the write side: the 55 copies of
/// this function — do they agree? Two already distinguished the cases
/// (`platform-domain`, `studio-domain`), which is how we know the shape is right
/// rather than invented here.
#[test]
fn a_read_loop_tells_end_of_body_from_a_failed_read() {
    let mut sloppy = Vec::new();

    // The definition, first and by name. 47 components expand it, so scanning it
    // only because `components/guestio` happens to be a guest source would make the
    // guard depend on a coincidence — the same reasoning as the write side.
    let macro_src = repo_root().join("components/guestio/src/lib.rs");
    match std::fs::read_to_string(&macro_src) {
        Ok(text) => {
            let body: String = text
                .split("macro_rules! guest_read_body")
                .nth(1)
                .unwrap_or_default()
                .chars()
                .take(2000)
                .collect();
            if !body.contains("StreamError::Closed") {
                sloppy.push(
                    "  components/guestio: the macro does not tell end-of-body from a \
                     failed read"
                        .to_string(),
                );
            }
            if !body.contains("$limit") {
                sloppy.push(
                    "  components/guestio: the macro does not bound what it reads".to_string(),
                );
            }
        }
        Err(e) => sloppy.push(format!("  components/guestio/src/lib.rs is unreadable ({e})")),
    }

    for path in guest_sources() {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.contains("blocking_read") {
                continue;
            }
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            // The window is the loop body that follows the read.
            let to = (i + 14).min(lines.len());
            let window = lines[i..to].join("\n");
            let distinguishes = window.contains("StreamError::Closed");
            // `while let Ok(..)` has no error arm to distinguish anything in.
            let swallows = line.contains("while let Ok(");
            if !distinguishes && (swallows || window.contains("Err(_) => break")) {
                let rel = path
                    .strip_prefix(repo_root().join("components"))
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                if READS_ALLOWED.iter().any(|(site, _)| rel.starts_with(site)) {
                    continue;
                }
                sloppy.push(format!("  {rel}:{}", i + 1));
            }
        }
    }
    assert!(
        sloppy.is_empty(),
        "these treat a failed read as the end of the body, so a truncated payload \
         arrives looking complete:\n{}\n\nMatch `Err(StreamError::Closed)` for the \
         end and handle the other arm, or add the site to READS_ALLOWED with the \
         reason truncation is harmless there.",
        sloppy.join("\n")
    );
}

/// The helper itself, wherever it appears, has to be the real one.
///
/// A `write_all` that loops on a constant would pass the test above while
/// reintroducing the flush-per-4KB it was written to avoid — and one that forgets
/// the `Ok(0)` case would spin instead of waiting.
///
/// Most components no longer write their own: `guestio::guest_write_all!()` expands
/// it, and 49 byte-identical copies collapsed into that one definition. So the
/// definition is checked FIRST and by name — without this, a file that stopped
/// containing `fn write_all(` would simply stop being checked, and the guard would
/// have lapsed at exactly the moment it became load-bearing for fifty crates.
#[test]
fn every_write_all_asks_the_stream_how_much_it_will_take() {
    let mut wrong = Vec::new();

    let macro_src = repo_root().join("components/guestio/src/lib.rs");
    match std::fs::read_to_string(&macro_src) {
        Ok(text) => {
            let body: String =
                text.split("macro_rules! guest_write_all").nth(1).unwrap_or_default().chars().take(1600).collect();
            if !body.contains("check_write") {
                wrong.push("  components/guestio: the macro does not call check_write".to_string());
            }
            if !body.contains("subscribe") {
                wrong.push("  components/guestio: the macro does not wait when the stream is full".to_string());
            }
        }
        // Not "no macro, nothing to check": fifty components expand it, so its
        // absence is a broken tree rather than a component that opted out.
        Err(e) => wrong.push(format!("  components/guestio/src/lib.rs is unreadable ({e})")),
    }

    for path in guest_sources() {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        if !text.contains("fn write_all(") {
            continue;
        }
        let body: String =
            text.split("fn write_all(").nth(1).unwrap_or_default().chars().take(1200).collect();
        let rel = path.strip_prefix(repo_root()).unwrap_or(&path).display().to_string();
        if !body.contains("check_write") {
            wrong.push(format!("  {rel}: does not call check_write"));
        }
        if !body.contains("subscribe") {
            wrong.push(format!("  {rel}: does not wait when the stream is full"));
        }
    }
    assert!(wrong.is_empty(), "write_all is not the real one in:\n{}", wrong.join("\n"));
}

/// Components whose `read_body` may grow without a ceiling, and why.
///
/// Much shorter than `READS_ALLOWED`, because "a truncated body is harmless here"
/// is a claim about one handler's data and "an unbounded body is harmless here" is
/// a claim about the whole process's memory.
const UNBOUNDED_ALLOWED: &[(&str, &str)] = &[
    (
        "agent-probe",
        "reads its own test input over a loopback socket the suite writes; there is \
         no caller who could send it a large body",
    ),
    (
        "bench-suite-p3",
        "reads through `Request::consume_body().collect()`, an async API with no \
         chunk loop to put a ceiling in — bounding it means bounding it upstream",
    ),
];

/// A request body is read into memory, so something has to say how much.
///
/// 38 components declared a `MAX_BODY_BYTES` and 17 did not, which is an
/// inconsistency rather than a decision: `comp-host` does not bound request bodies
/// either, so an uncapped read grows until the store's memory cap traps the
/// component and the connection closes with no status at all. `upload-drop` — an
/// app whose entire purpose is accepting files — was one of the 17.
///
/// The capped ones already say it best in their own comment: *a ceiling, not a
/// policy*. 16 MiB is not a considered limit on what any particular handler should
/// accept; it is the difference between refusing a body and dying of one.
#[test]
fn a_body_read_into_memory_has_a_ceiling() {
    let mut unbounded = Vec::new();
    for path in guest_sources() {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        // Both spellings. 47 components stopped containing the literal when the
        // body moved into `guestio::guest_read_body!`, and a check keyed only on
        // the definition would have dropped them out of scope at exactly the
        // moment one definition started serving all of them.
        if !text.contains("fn read_body") && !text.contains("guest_read_body!") {
            continue;
        }
        if text.contains("MAX_BODY_BYTES") {
            continue;
        }
        let rel = path
            .strip_prefix(repo_root().join("components"))
            .unwrap_or(&path)
            .display()
            .to_string();
        if UNBOUNDED_ALLOWED.iter().any(|(site, _)| rel.starts_with(site)) {
            continue;
        }
        unbounded.push(format!("  {rel}"));
    }
    assert!(
        unbounded.is_empty(),
        "these read a request body into memory with no ceiling, so a large enough \
         request traps the component instead of being refused:\n{}\n\nAdd a \
         `MAX_BODY_BYTES` check to the read loop, or add the component to \
         UNBOUNDED_ALLOWED with the reason nothing can send it a large body.",
        unbounded.join("\n")
    );
}

/// A percent escape is a BYTE, and nothing may decode one straight into a `char`.
///
/// 28 decoders existed across the pool under six names, and seven of them were the
/// one-pass shape: `out.push(b as char)`, which reads a decoded byte as a Unicode
/// code point. Two separate bugs came out of it, and only the first is obvious:
///
///   * every multi-byte UTF-8 sequence came back as its bytes reinterpreted as
///     Latin-1 — `caf%C3%A9` decoded to `cafÃ©`.
///
///   * `u8::from_str_radix` accepts a leading sign, so `%+a` parsed as `+0x0a` and
///     emitted a NEWLINE where the correct shape leaves the literal text `% a`.
///     That is a caller injecting a control byte, not a display glitch. Found by
///     fuzzing the two shapes against each other — 200 000 inputs, 699 divergences
///     on pure ASCII, all of this form.
///
/// ASCII text decodes identically under both, which is exactly why it survived: it
/// is correct for the inputs a test written in English would use.
///
/// `guestfmt::percent_decode` is the byte-correct one, and replacing `+` before the
/// radix call is what closes the second hole rather than only the first.
#[test]
fn no_component_decodes_a_percent_escape_into_a_char() {
    let mut wrong = Vec::new();
    for path in guest_sources() {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || !trimmed.contains("from_str_radix") {
                continue;
            }
            // The window is the arm that consumes the parsed byte.
            let to = (i + 6).min(lines.len());
            let window = lines[i..to].join("\n");
            if window.contains("as char") {
                let rel = path
                    .strip_prefix(repo_root().join("components"))
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                wrong.push(format!("  {rel}:{}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "these decode a percent escape into a `char`, which mangles every non-ASCII \
         character and turns `%+a` into a newline:\n{}\n\nUse \
         `guestfmt::percent_decode`, which collects bytes and decodes once.",
        wrong.join("\n")
    );
}

/// An `Authorization` scheme name is matched case-insensitively.
///
/// RFC 7235 §2.1 says the scheme name is case-insensitive, and 22 of 24 hand-written
/// copies matched the literal `"Bearer "`. A client sending `authorization: bearer
/// <token>` — which is legal, and which some HTTP libraries normalise to — got a 401
/// with nothing to explain it. Sixteen also left whitespace attached to the
/// credential, so one trailing space authenticated against some components and not
/// others.
///
/// `guestio::guest_bearer!()` finds the header and `guestfmt::bearer_token` parses
/// the value, which is where the unit tests for all of this live. This catches a
/// component going back to doing it by hand.
#[test]
fn no_component_matches_a_bearer_scheme_case_sensitively() {
    let mut wrong = Vec::new();
    for path in guest_sources() {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for (i, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            // The literal, in either casing — matching ONE casing is the bug, whichever
            // one it is.
            if !trimmed.contains(r#"strip_prefix("Bearer "#)
                && !trimmed.contains(r#"strip_prefix("bearer "#)
            {
                continue;
            }
            let rel = path
                .strip_prefix(repo_root().join("components"))
                .unwrap_or(&path)
                .display()
                .to_string();
            if BEARER_ALLOWED.iter().any(|(site, _)| rel.starts_with(site)) {
                continue;
            }
            wrong.push(format!("  {rel}:{}", i + 1));
        }
    }
    assert!(
        wrong.is_empty(),
        "these match an Authorization scheme by literal prefix, so a legal \
         `authorization: bearer <token>` is refused:\n{}\n\nUse \
         `guestio::guest_bearer!()`, which parses the value with \
         `guestfmt::bearer_token`.",
        wrong.join("\n")
    );
}

/// Components that parse an `Authorization` value themselves, and why.
const BEARER_ALLOWED: &[(&str, &str)] = &[(
    "conduit-domain",
    "the RealWorld spec sends `Authorization: Token <jwt>`, not Bearer, so this one \
     accepts a second scheme and is not the shared shape",
), (
    "clinic-domain",
    "returns a `String` rather than an `Option`, so an absent credential and an \
     empty one are the same value to every caller — a change worth making on its \
     own rather than inside a mechanical substitution",
)];
