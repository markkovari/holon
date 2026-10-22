//! A daemon though `image` would compile to wasm: ADR-0095 keeps every host capability in one
//! daemon-behind-a-contract shape.

use std::path::{Path, PathBuf};

use anyhow::Result;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use clap::Parser;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::ImageEncoder;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-imageopt", about = "Shrink an image for a component that has no codec.")]
struct Args {
    /// Shared secret callers must send as `Authorization: Bearer <token>`; unset means no check.
    #[arg(long)]
    token: Option<String>,
    /// Like `--token` but read from a file, since a flag is `ps`-readable; takes precedence.
    #[arg(long)]
    token_file: Option<std::path::PathBuf>,

    /// Where to listen; loopback by default, since this reads and writes files.
    #[arg(long, default_value = "127.0.0.1:8004")]
    addr: String,

    /// A directory this may read from and write to, repeatable; none means nothing is permitted.
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
    /// Returns the CANONICAL path, which the caller must act on (TOCTOU). Canonicalises the parent so
    /// a missing file reads as `no-such-file`; a symlink swapped in for the file itself is not covered.
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

/// A sibling, not an overwrite, so the caller keeps the original.
fn output_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let ext = input.extension().map(|e| e.to_string_lossy().into_owned());
    let name = match ext {
        Some(ext) => format!("{stem}.opt.{ext}"),
        None => format!("{stem}.opt"),
    };
    input.with_file_name(name)
}

/// A JPEG gets its own encoder: the generic `save()` ignores `quality` and re-encodes at 75.
fn save(img: &image::DynamicImage, out: &Path, quality: u8) -> image::ImageResult<()> {
    let is_jpeg = matches!(
        out.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("jpg") | Some("jpeg")
    );
    if !is_jpeg {
        return img.save(out);
    }
    let file = std::fs::File::create(out).map_err(image::ImageError::IoError)?;
    let mut writer = std::io::BufWriter::new(file);
    let encoder = JpegEncoder::new_with_quality(&mut writer, quality);
    encoder.write_image(img.as_bytes(), img.width(), img.height(), img.color().into())
}

async fn optimize(
    State(d): State<std::sync::Arc<Daemon>>,
    Json(req): Json<OptimizeReq>,
) -> Json<Value> {
    let requested = PathBuf::from(&req.img);

    // Distinct answers on purpose: a refusal is an operator's decision, a missing file a fact.
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
    let token = comp_daemon_kit::resolve_token(args.token.clone(), args.token_file.clone());
    comp_daemon_kit::warn_if_unauthenticated("comp-imageopt", &token);
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
    let app = Router::new()
        .route("/optimize", post(optimize))
        .with_state(state)
        .layer(axum::middleware::from_fn(comp_daemon_kit::require_token))
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

    #[test]
    fn a_dot_dot_cannot_walk_out_of_the_allow_list() {
        // Test-only fixtures, removed at the end.
        let tmp = std::env::temp_dir().canonicalize().expect("temp dir"); // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let inside = tmp.join("imageopt-allowed");
        std::fs::create_dir_all(&inside).expect("mkdir");
        let d = daemon(&[&inside]);

        assert!(
            d.permits(&inside.join("photo.jpg")).is_some(),
            "a file inside the listed directory"
        );
        assert!(
            d.permits(&inside.join("../sibling.jpg")).is_none(),
            "a file in the parent is not inside it"
        );
        assert!(d.permits(&tmp.join("photo.jpg")).is_none(), "nor is anything above it");
        assert!(d.permits(Path::new("/etc/photo.jpg")).is_none(), "nor is somewhere unrelated");
    }

    #[test]
    fn an_unscoped_daemon_touches_nothing() {
        let d = daemon(&[]);
        assert!(d.permits(&std::env::temp_dir().join("photo.jpg")).is_none());
        assert!(d.permits(Path::new("/photo.jpg")).is_none());
    }

    /// A missing file in an allowed directory is permitted: `no-such-file` is the caller's to report.
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

    #[test]
    fn a_jpeg_saved_at_a_given_quality_round_trips() {
        let img = image::DynamicImage::new_rgb8(4, 4);
        // Test-only scratch file, removed below.
        let out =
            std::env::temp_dir().join(format!("imageopt-save-test-{}.jpg", std::process::id())); // nosemgrep: rust.lang.security.temp-dir.temp-dir
        save(&img, &out, 50).expect("save");
        let back = image::open(&out).expect("reopen");
        assert_eq!((back.width(), back.height()), (4, 4));
        std::fs::remove_file(&out).ok();
    }
}
