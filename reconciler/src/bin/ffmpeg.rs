//! `comp-ffmpeg` — re-encode a video, for a component that cannot fork a process
//!
//! ## Why this is a process and not a component
//!
//! Transcoding needs to spawn `ffmpeg`, and a `wasm32-wasip2` guest cannot fork
//! a process. That is the sandbox working rather than a gap to route around
//! (ADR-0095), so the part that needs an operating system is native and is
//! reached over HTTP exactly like the watcher, the gate and the database are.
//!
//! `components/video-ffmpeg` is the component side: it holds the WIT contract
//! and dials this. Nothing here knows what a goal is.
//!
//! ## An allow-list, for the same reason `comp-fswatch` has one
//!
//! The input path in a request can come from a model. So a request names a
//! file and this refuses it unless the file's directory was listed by an
//! operator. `--allow-path /var/media` permits `/var/media` and anything
//! beneath it. Nothing is permitted by default; a daemon started with no
//! `--allow-path` refuses everything, which is the correct behaviour for a
//! capability nobody has scoped yet.
//!
//! ## The target format
//!
//! The WIT contract takes one string argument, so there is no room for a
//! second "output format" parameter without inventing one nobody asked for.
//! The target is fixed: MP4, alongside the input with the same stem — an
//! input `/x/y/clip.mov` becomes `/x/y/clip.mp4`.
//!
//!   comp-ffmpeg --addr 127.0.0.1:8010 --allow-path /var/media

use std::path::{Path, PathBuf};

use anyhow::Result;
use axum::{extract::State, routing::post, Json, Router};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-ffmpeg", about = "Native daemon for video-ffmpeg")]
struct Args {
    /// Where to listen. Loopback by default: this runs ffmpeg on request and
    /// has no authentication of its own.
    #[arg(long, default_value = "127.0.0.1:8010")]
    addr: String,

    /// A directory this may read input from, repeatable.
    ///
    /// An allow-list rather than a root to chroot into, for the same reason
    /// egress is an allow-list: the input comes from an agent. Empty means
    /// nothing is permitted, which is what a capability nobody has scoped
    /// should do.
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
    /// Is `path` inside something an operator listed?
    ///
    /// Canonicalised on both sides before comparing, so `/var/media/../etc`
    /// is judged as `/etc` — a prefix test on the string a caller sent would
    /// let `..` walk straight out of the allow-list.
    fn permits(&self, path: &Path) -> bool {
        let Ok(real) = path.canonicalize() else { return false };
        self.allowed.iter().any(|a| a.canonicalize().map(|a| real.starts_with(a)).unwrap_or(false))
    }
}

/// The fixed output path for a given input: same directory and stem, `.mp4`
/// extension. A fixed target rather than a second parameter, because the WIT
/// contract only carries one string.
fn output_path(input: &Path) -> PathBuf {
    input.with_extension("mp4")
}

async fn transcode(State(d): State<std::sync::Arc<Daemon>>, Json(req): Json<TranscodeReq>) -> Json<Value> {
    let input = PathBuf::from(&req.input);

    // Not-permitted and no-such-file are different answers on purpose: one is
    // a decision an operator made and the other is a fact about the disk, and
    // a caller retrying a refusal forever is the worse mistake.
    if !d.permits(&input) {
        return Json(json!({ "error": "not-permitted", "detail": req.input }));
    }
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
        Ok(s) if s.success() && output.is_file() => {
            Json(serde_json::to_value(TranscodeResp { output: output.display().to_string() }).unwrap())
        }
        Ok(s) => Json(json!({ "error": "unavailable", "detail": format!("ffmpeg exited {s}") })),
        Err(e) => Json(json!({ "error": "unavailable", "detail": format!("failed to run ffmpeg: {e}") })),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.allow_path.is_empty() {
        eprintln!(
            "comp-ffmpeg: no --allow-path given, so every request will be refused. \
             That is deliberate — a transcoder nobody has scoped reads nothing."
        );
    }
    let allowed = args.allow_path.clone();
    println!("comp-ffmpeg: listening on http://{} | {} allowed path(s)", args.addr, allowed.len());
    let state = std::sync::Arc::new(Daemon { allowed });
    let app = Router::new().route("/transcode", post(transcode)).with_state(state);
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

    /// The allow-list is the boundary, and `..` is how a path escapes one.
    #[test]
    fn a_dot_dot_cannot_walk_out_of_the_allow_list() {
        let tmp = std::env::temp_dir().canonicalize().expect("temp dir");
        let inside = tmp.join("ffmpeg-allowed");
        std::fs::create_dir_all(&inside).expect("mkdir");
        let d = daemon(&[&inside]);

        assert!(d.permits(&inside), "the listed directory itself");
        assert!(!d.permits(&inside.join("..")), "the parent is not inside it");
        assert!(!d.permits(&tmp), "nor is anything above it");
        assert!(!d.permits(Path::new("/etc")), "nor is somewhere unrelated");
    }

    /// Nothing is permitted by default.
    #[test]
    fn an_unscoped_daemon_reads_nothing() {
        let d = daemon(&[]);
        assert!(!d.permits(&std::env::temp_dir()));
        assert!(!d.permits(Path::new("/")));
    }

    /// The output path is derived, not invented: same directory and stem,
    /// `.mp4` extension.
    #[test]
    fn the_output_path_swaps_the_extension_to_mp4() {
        assert_eq!(output_path(Path::new("/x/y/clip.mov")), PathBuf::from("/x/y/clip.mp4"));
        assert_eq!(output_path(Path::new("/x/y/clip")), PathBuf::from("/x/y/clip.mp4"));
        assert_eq!(output_path(Path::new("clip.webm")), PathBuf::from("clip.mp4"));
    }
}
