//! photo-critic, live on the lattice: deploy the component, serve the UI, and
//! POST an image to /evaluate — which reaches Claude's vision API by egress with
//! the key from the vault, and returns a real critique over the lattice.
//!
//! Ignored by default: it spends money and needs a real key. Run explicitly:
//!   cargo test --release --test photo_critic_live -- --ignored --nocapture

use std::time::{Duration, Instant};

use comp_reconciler::fleet::{free_port, repo_root, Fleet};
use serde_json::Value;

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let p = dir.join("photo_critic.wasm");
    assert!(p.exists(), "missing {} — build photo-critic first", p.display());
    vec![format!("gate={}", p.display())]
}

/// A small but real gradient PNG, so there is an actual image to critique.
fn test_png() -> Vec<u8> {
    let (w, h) = (320u32, 240u32);
    let mut rows: Vec<u8> = Vec::new();
    for y in 0..h {
        rows.push(0); // filter byte per scanline
        for x in 0..w {
            rows.push((x * 255 / w) as u8);
            rows.push((y * 255 / h) as u8);
            rows.push(128);
        }
    }
    fn chunk(t: &[u8], d: &[u8]) -> Vec<u8> {
        let mut out = (d.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(t);
        out.extend_from_slice(d);
        let mut crc = t.to_vec();
        crc.extend_from_slice(d);
        out.extend_from_slice(&crc32(&crc).to_be_bytes());
        out
    }
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let idat = miniz_oxide::deflate::compress_to_vec_zlib(&rows, 6);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(chunk(b"IHDR", &ihdr));
    png.extend(chunk(b"IDAT", &idat));
    png.extend(chunk(b"IEND", &[]));
    png
}

#[test]
#[ignore]
fn upload_a_photo_and_get_a_critique_over_the_lattice() {
    let key_path = dirs_home().join(".comp-secrets/anthropic");
    assert!(key_path.exists(), "need ~/.comp-secrets/anthropic");
    let _ = free_port();

    let fleet = Fleet::start_with_secrets(
        "photo",
        &[repo_root().join("fixtures/photo-critic.yaml").to_str().unwrap()],
        &artifacts(),
        &[format!("vault://acme/anthropic=@{}", key_path.display())],
    );

    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(90)).build().unwrap();
    let base = format!("http://127.0.0.1:{}", fleet.ingress_port);

    // 1) the UI serves over the lattice
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut served = false;
    while Instant::now() < deadline {
        if let Ok(r) = http.get(&base).header("host", "photo.acme.test").send() {
            if r.status().is_success() && r.text().unwrap_or_default().contains("Photo Critic") {
                served = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(served, "the UI never served\n{}", fleet.node_log("n1"));
    println!("    UI is live over the lattice");

    // 2) upload the image; the component egresses to Claude vision and returns a critique
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(test_png());
    let body = serde_json::json!({ "media_type": "image/png", "data": b64 });
    let r = http
        .post(format!("{base}/evaluate"))
        .header("host", "photo.acme.test")
        .json(&body)
        .send()
        .expect("evaluate");
    let status = r.status();
    let v: Value = r.json().unwrap_or(Value::Null);
    assert!(status.is_success(), "evaluate failed: {status} {v}\n{}", fleet.node_log("n1"));
    let critique = v["critique"].as_str().unwrap_or("");
    assert!(critique.contains("Interesting") || critique.contains("Composition"),
        "no critique came back: {v}");
    println!("    critique over the lattice:\n{}", critique);
}

fn dirs_home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap())
}
