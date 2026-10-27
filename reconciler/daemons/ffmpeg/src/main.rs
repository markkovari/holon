//! Native side of components/video-ffmpeg (ADR-0095): a wasm guest cannot spawn `ffmpeg`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "holon-ffmpeg", about = "Native daemon for video-ffmpeg")]
struct Args {
    /// Shared secret callers must send as `Authorization: Bearer <token>`; unset means no check.
    #[arg(long)]
    token: Option<String>,
    /// Like `--token` but read from a file, since a flag is `ps`-readable; takes precedence.
    #[arg(long)]
    token_file: Option<std::path::PathBuf>,

    /// Where to listen; loopback by default, since this runs ffmpeg on request.
    #[arg(long, default_value = "127.0.0.1:8010")]
    addr: String,

    /// A directory this may read input from, repeatable; none means nothing is permitted.
    #[arg(long = "allow-path")]
    allow_path: Vec<PathBuf>,
}

struct Daemon {
    allowed: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct TranscodeReq {
    input: String,
}

#[derive(Serialize)]
struct TranscodeResp {
    output: String,
}

impl Daemon {
    /// Returns the CANONICAL path, which the caller must act on (TOCTOU). Canonicalises the parent so
    /// a missing file reads as `no-such-file`, not `not-permitted`.
    fn permits(&self, path: &Path) -> Option<PathBuf> {
        let dir = path.parent()?;
        let name = path.file_name()?;
        let real_dir = dir.canonicalize().ok()?;
        let permitted = self
            .allowed
            .iter()
            .any(|a| a.canonicalize().map(|a| real_dir.starts_with(a)).unwrap_or(false));
        permitted.then(|| real_dir.join(name))
    }
}

/// A fixed target: the WIT contract carries only one string.
fn output_path(input: &Path) -> PathBuf {
    input.with_extension("mp4")
}

async fn transcode(
    State(d): State<std::sync::Arc<Daemon>>,
    Json(req): Json<TranscodeReq>,
) -> Json<Value> {
    let requested = PathBuf::from(&req.input);

    // Distinct answers on purpose: a refusal is an operator's decision, a missing file a fact.
    let Some(input) = d.permits(&requested) else {
        return Json(json!({ "error": "not-permitted", "detail": req.input }));
    };
    if !input.is_file() {
        return Json(json!({ "error": "no-such-file", "detail": req.input }));
    }

    let output = output_path(&input);
    let status = tokio::process::Command::new("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(&input)
        .arg(&output)
        .status()
        .await;

    match status {
        Ok(s) if s.success() && output.is_file() => Json(
            serde_json::to_value(TranscodeResp { output: output.display().to_string() }).unwrap(),
        ),
        Ok(s) => Json(json!({ "error": "unavailable", "detail": format!("ffmpeg exited {s}") })),
        Err(e) => {
            Json(json!({ "error": "unavailable", "detail": format!("failed to run ffmpeg: {e}") }))
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = holon_daemon_kit::resolve_token(args.token.clone(), args.token_file.clone());
    holon_daemon_kit::warn_if_unauthenticated("holon-ffmpeg", &token);
    if args.allow_path.is_empty() {
        eprintln!(
            "holon-ffmpeg: no --allow-path given, so every request will be refused. \
             That is deliberate — a transcoder nobody has scoped reads nothing."
        );
    }
    let allowed = args.allow_path.clone();
    println!("holon-ffmpeg: listening on http://{} | {} allowed path(s)", args.addr, allowed.len());
    let state = std::sync::Arc::new(Daemon { allowed });
    let app = Router::new()
        .route("/transcode", post(transcode))
        .with_state(state)
        .layer(axum::middleware::from_fn(holon_daemon_kit::require_token))
        .layer(axum::Extension(std::sync::Arc::new(token)));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon(allow: &[&Path]) -> Daemon {
        Daemon { allowed: allow.iter().map(|p| p.to_path_buf()).collect() }
    }

    #[test]
    fn a_dot_dot_cannot_walk_out_of_the_allow_list() {
        // Test-only fixtures, removed at the end.
        let tmp = std::env::temp_dir().canonicalize().expect("temp dir"); // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let inside = tmp.join("ffmpeg-allowed");
        std::fs::create_dir_all(&inside).expect("mkdir");
        let d = daemon(&[&inside]);

        assert!(
            d.permits(&inside.join("input.avi")).is_some(),
            "a file inside the listed directory"
        );
        assert!(
            d.permits(&inside.join("../sibling.avi")).is_none(),
            "a file in the parent is not inside it"
        );
        assert!(d.permits(&tmp.join("input.avi")).is_none(), "nor is anything above it");
        assert!(d.permits(Path::new("/etc/input.avi")).is_none(), "nor is somewhere unrelated");
    }

    #[test]
    fn an_unscoped_daemon_reads_nothing() {
        let d = daemon(&[]);
        assert!(d.permits(&std::env::temp_dir().join("input.avi")).is_none());
        assert!(d.permits(Path::new("/input.avi")).is_none());
    }

    /// A missing file in an allowed directory is permitted: `no-such-file` is the caller's to report.
    #[test]
    fn a_missing_file_in_an_allowed_directory_is_still_permitted() {
        let tmp = std::env::temp_dir().canonicalize().expect("temp dir"); // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let inside = tmp.join(format!("ffmpeg-missing-{}", std::process::id()));
        std::fs::create_dir_all(&inside).expect("mkdir");
        let d = daemon(&[&inside]);
        let missing = inside.join("never-written.avi");
        assert!(!missing.exists());
        assert_eq!(d.permits(&missing), Some(missing));
        std::fs::remove_dir_all(&inside).ok();
    }

    #[test]
    fn the_output_path_swaps_the_extension_to_mp4() {
        assert_eq!(output_path(Path::new("/x/y/clip.mov")), PathBuf::from("/x/y/clip.mp4"));
        assert_eq!(output_path(Path::new("/x/y/clip")), PathBuf::from("/x/y/clip.mp4"));
        assert_eq!(output_path(Path::new("clip.webm")), PathBuf::from("clip.mp4"));
    }
}
