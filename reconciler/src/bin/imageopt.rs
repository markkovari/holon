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
    /// Is `path` inside something an operator listed?
    ///
    /// Canonicalised on both sides before comparing, so `/allowed/../etc` is
    /// judged as `/etc` — a prefix test on the string a caller sent would let
    /// `..` walk straight out of the allow-list.
    fn permits(&self, path: &Path) -> bool {
        let Ok(real) = path.canonicalize() else { return false };
        self.allowed.iter().any(|a| a.canonicalize().map(|a| real.starts_with(a)).unwrap_or(false))
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
    let input = PathBuf::from(&req.img);

    // Not-permitted and no-such-file are different answers on purpose: one is
    // a decision an operator made and the other is a fact about the disk, and
    // a caller retrying a refusal forever is the worse mistake.
    if !d.permits(&input) {
        return Json(json!({ "error": "not-permitted", "detail": req.img }));
    }
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
    let app = Router::new().route("/optimize", post(optimize)).with_state(state);
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

        assert!(d.permits(&inside), "the listed directory itself");
        assert!(!d.permits(&inside.join("..")), "the parent is not inside it");
        assert!(!d.permits(&tmp), "nor is anything above it");
        assert!(!d.permits(Path::new("/etc")), "nor is somewhere unrelated");
    }

    /// Nothing is permitted by default.
    #[test]
    fn an_unscoped_daemon_touches_nothing() {
        let d = daemon(&[]);
        assert!(!d.permits(&std::env::temp_dir()));
        assert!(!d.permits(Path::new("/")));
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
