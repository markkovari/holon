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
    (
        "photo-critic",
        "a truncated image fails to decode; the failure is loud and immediate",
    ),
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
#[test]
fn every_write_all_asks_the_stream_how_much_it_will_take() {
    let mut wrong = Vec::new();
    for path in guest_sources() {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        if !text.contains("fn write_all(") {
            continue;
        }
        let body: String = text
            .split("fn write_all(")
            .nth(1)
            .unwrap_or_default()
            .chars()
            .take(1200)
            .collect();
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
