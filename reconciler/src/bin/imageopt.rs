//! `comp-imageopt` — shrink a picture, for a component that has no codec.
//!
//! ## Why this is a process and not a component, and the tension worth naming
//!
//! ADR-0095's own text motivates a native daemon here with "re-encoding an
//! image needs an image codec and CPU time" — but pure-Rust image codecs (this
//! file uses the `image` crate) compile to `wasm32-wasip2` just fine, and a
//! wasm guest has CPU time same as any other code. Read strictly, this
//! capability could be a component like any other.
//!
//! It is a daemon anyway, because ADR-0095 commits all twelve host
//! capabilities to the same daemon-behind-a-contract shape "so
//! `container-docker` and `ui-notifier` do not deserve the same blast radius,
//! and one daemon for all twelve would give them one" — picking one-off
//! exceptions per capability is how that boundary erodes. So this follows the
//! pattern for consistency with the accepted ADR, not because its own
//! three-question test strictly requires it for this one capability.
//!
//! ## An allow-list, for the same reason `comp-fswatch` has one
//!
//! The path in a request can come from a model. Nothing is permitted by
//! default; `--allow-path /var/photos` permits that directory and anything
//! beneath it, canonicalised on both sides before comparing so `..` cannot
//! walk out.
//!
//!   comp-imageopt --addr 127.0.0.1:8004 --allow-path /var/photos --max-width 1600 --quality 80

use std::path::{Path, PathBuf};

use anyhow::Result;
use axum::{extract::State, routing::post, Json, Router};
use clap::Parser;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::ImageEncoder;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-imageopt", about = "Shrink an image for a component that has no codec.")]
struct Args {
    /// Shared secret a caller must send as `Authorization: Bearer
    /// <token>`. Loopback binding alone is not a boundary — see
    /// `comp_reconciler::daemon_auth`'s own doc for why. No token means
    /// no check, logged loudly rather than silently.
    #[arg(long)]
    token: Option<String>,
    /// Same, but read from a file (a systemd `LoadCredential` path)
    /// rather than passed as a value — `--token` is `ps`-readable by
    /// any local user, which is most of what this exists to close.
    /// Wins over `--token` when both are given.
    #[arg(long)]
    token_file: Option<std::path::PathBuf>,

    /// Where to listen. Loopback by default: this reads and writes files and
    /// has no authentication of its own.
    #[arg(long, default_value = "127.0.0.1:8004")]
    addr: String,

    /// A directory this may read from and write to, repeatable. Empty means
    /// nothing is permitted, which is what a capability nobody has scoped
    /// should do.
    #[arg(long = "allow-path")]
    allow_path: Vec<PathBuf>,

    /// An image wider than this is resized down, preserving aspect ratio.
    #[arg(long, default_value_t = 1600)]
    max_width: u32,

    /// Compression quality, used only when the output is a JPEG.
    #[arg(long, default_value_t = 80)]
    quality: u8,
}

struct Daemon {
    allowed: Vec<PathBuf>,
    max_width: u32,
    quality: u8,
}

impl Daemon {
    /// Is `path` inside something an operator listed? Returns the CANONICAL
    /// path when it is — the caller must act on that, not the path it was
    /// given, or a symlink swapped in between this check and the read that
    /// follows it resolves to somewhere this check never approved (TOCTOU).
    ///
    /// Canonicalises the PARENT directory rather than `path` itself: `path`
    /// (a file, not `fs-watcher`'s directory) need not exist yet for this
    /// check — a missing file is `no-such-file`, not `not-permitted`, and
    /// canonicalizing the whole path would fail for both the same way,
    /// collapsing that distinction. The parent must exist and be real; a
    /// symlink swapped in for the final component between this check and
    /// `image::open` is a narrower race this does not close.
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

#[derive(Deserialize)]
struct OptimizeReq {
    img: String,
}

/// `/x/y/photo.jpg` -> `/x/y/photo.opt.jpg`. A sibling file rather than an
/// overwrite, so a caller keeps the original to compare against or fall back
/// to.
fn output_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let ext = input.extension().map(|e| e.to_string_lossy().into_owned());
    let name = match ext {
        Some(ext) => format!("{stem}.opt.{ext}"),
        None => format!("{stem}.opt"),
    };
    input.with_file_name(name)
}

/// Save `img` to `out`, format-preserving — except a JPEG output, where the
/// crate's generic `save()` would ignore `quality` and re-encode at its
/// default (75). A JPEG is the one format this daemon has a quality knob for.
fn save(img: &image::DynamicImage, out: &Path, quality: u8) -> image::ImageResult<()> {
    let is_jpeg = matches!(
        out.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("jpg") | Some("jpeg")
    );
    if !is_jpeg {
        return img.save(out);
    }
    let file = std::fs::File::create(out)
        .map_err(|e| image::ImageError::IoError(e))?;
    let mut writer = std::io::BufWriter::new(file);
    let encoder = JpegEncoder::new_with_quality(&mut writer, quality);
    encoder.write_image(img.as_bytes(), img.width(), img.height(), img.color().into())
}

async fn optimize(State(d): State<std::sync::Arc<Daemon>>, Json(req): Json<OptimizeReq>) -> Json<Value> {
    let requested = PathBuf::from(&req.img);

    // Not-permitted and no-such-file are different answers on purpose: one is
    // a decision an operator made and the other is a fact about the disk, and
    // a caller retrying a refusal forever is the worse mistake.
    //
    // Act on the CANONICAL path `permits` approved, not the one the caller
    // sent — see `permits`'s own doc for why.
    let Some(input) = d.permits(&requested) else {
        return Json(json!({ "error": "not-permitted", "detail": req.img }));
    };
    if !input.is_file() {
        return Json(json!({ "error": "no-such-file", "detail": req.img }));
    }

    let img = match image::open(&input) {
        Ok(img) => img,
        Err(e) => return Json(json!({ "error": "unavailable", "detail": e.to_string() })),
    };

    let resized = if img.width() > d.max_width {
        let ratio = d.max_width as f64 / img.width() as f64;
        let height = (img.height() as f64 * ratio).round().max(1.0) as u32;
        img.resize(d.max_width, height, FilterType::Lanczos3)
    } else {
        img
    };

    let out = output_path(&input);
    if let Err(e) = save(&resized, &out, d.quality) {
        return Json(json!({ "error": "unavailable", "detail": e.to_string() }));
    }

    Json(json!({ "output": out.display().to_string() }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = comp_reconciler::daemon_auth::resolve_token(args.token.clone(), args.token_file.clone());
    comp_reconciler::daemon_auth::warn_if_unauthenticated("comp-imageopt", &token);
    if args.allow_path.is_empty() {
        eprintln!(
            "comp-imageopt: no --allow-path given, so every request will be refused. \
             That is deliberate — a daemon nobody has scoped touches nothing."
        );
    }
    println!(
        "comp-imageopt: listening on http://{} | {} allowed path(s) | max-width {} | quality {}",
        args.addr,
        args.allow_path.len(),
        args.max_width,
        args.quality
    );
    let state = std::sync::Arc::new(Daemon {
        allowed: args.allow_path,
        max_width: args.max_width,
        quality: args.quality,
    });
    let app = Router::new().route("/optimize", post(optimize)).with_state(state)
        .layer(axum::middleware::from_fn(comp_reconciler::daemon_auth::require_token))
        .layer(axum::Extension(std::sync::Arc::new(token)));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon(allow: &[&Path]) -> Daemon {
        Daemon {
            allowed: allow.iter().map(|p| p.to_path_buf()).collect(),
            max_width: 1600,
            quality: 80,
        }
    }

    /// The allow-list is the boundary, and `..` is how a path escapes one.
    #[test]
    fn a_dot_dot_cannot_walk_out_of_the_allow_list() {
        // Test scaffolding, not a security-sensitive file: the path only ever
        // holds this test's own fixtures, torn down at the end of it. Same
        // pattern as fs-watcher's daemon test.
        let tmp = std::env::temp_dir().canonicalize().expect("temp dir"); // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let inside = tmp.join("imageopt-allowed");
        std::fs::create_dir_all(&inside).expect("mkdir");
        let d = daemon(&[&inside]);

        assert!(d.permits(&inside.join("photo.jpg")).is_some(), "a file inside the listed directory");
        assert!(d.permits(&inside.join("../sibling.jpg")).is_none(), "a file in the parent is not inside it");
        assert!(d.permits(&tmp.join("photo.jpg")).is_none(), "nor is anything above it");
        assert!(d.permits(Path::new("/etc/photo.jpg")).is_none(), "nor is somewhere unrelated");
    }

    /// Nothing is permitted by default.
    #[test]
    fn an_unscoped_daemon_touches_nothing() {
        let d = daemon(&[]);
        assert!(d.permits(&std::env::temp_dir().join("photo.jpg")).is_none());
        assert!(d.permits(Path::new("/photo.jpg")).is_none());
    }

    /// `permits` checks the PARENT directory, not the file itself — a
    /// missing file inside an allowed directory must come back permitted
    /// (`no-such-file` is the caller's job to report), not `not-permitted`.
    /// Canonicalizing the whole path instead would fail identically for
    /// both, collapsing a real distinction the WIT contract makes.
    #[test]
    fn a_missing_file_in_an_allowed_directory_is_still_permitted() {
        let tmp = std::env::temp_dir().canonicalize().expect("temp dir"); // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let inside = tmp.join(format!("imageopt-missing-{}", std::process::id()));
        std::fs::create_dir_all(&inside).expect("mkdir");
        let d = daemon(&[&inside]);
        let missing = inside.join("never-written.jpg");
        assert!(!missing.exists());
        assert_eq!(d.permits(&missing), Some(missing));
        std::fs::remove_dir_all(&inside).ok();
    }

    #[test]
    fn the_output_name_inserts_opt_before_the_extension() {
        assert_eq!(output_path(Path::new("/x/y/photo.jpg")), PathBuf::from("/x/y/photo.opt.jpg"));
        assert_eq!(output_path(Path::new("/a/no-ext")), PathBuf::from("/a/no-ext.opt"));
    }

    /// The JPEG branch of `save()` builds its own encoder rather than calling
    /// the crate's generic `save()` — worth a real round trip rather than
    /// trusting it compiles.
    #[test]
    fn a_jpeg_saved_at_a_given_quality_round_trips() {
        let img = image::DynamicImage::new_rgb8(4, 4);
        // A scratch output file for this test only, named with the process id
        // and removed below; nothing security-sensitive is ever written to a
        // predictable path here.
        let out = std::env::temp_dir().join(format!("imageopt-save-test-{}.jpg", std::process::id())); // nosemgrep: rust.lang.security.temp-dir.temp-dir
        save(&img, &out, 50).expect("save");
        let back = image::open(&out).expect("reopen");
        assert_eq!((back.width(), back.height()), (4, 4));
        std::fs::remove_file(&out).ok();
    }
}
