//! The fake `comp-media` and the helpers every photoquest gate shares — moved out
//! of `gate_photoquest.rs` so the game's gates (quests, competitions, moderation)
//! judge against the same daemon. See `gate_photoquest.rs` for why it is fake.
#![allow(dead_code)]

use super::gatelib::Gate;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

pub const CRATE: &str = "photoquest-domain";
pub const SECRET: &str = "gate-callback-secret";
pub const TOKEN: &str = "gate-media-token";
pub const CALLBACK_BASE: &str = "http://photoquest.test";
/// Mirrors the daemon's `--max-upload-mb` default, so a refusal can be arranged.
pub const MAX_BYTES: u64 = 512 * 1024 * 1024;
pub const PART: u64 = 16 * 1024 * 1024;

/// One request the fake daemon received.
#[derive(Clone, Debug)]
pub struct Call {
    pub path: String,
    pub auth: String,
    pub body: Value,
}

/// `comp-media`, minus the store, the queue and the GPU.
pub struct FakeMedia {
    pub port: u16,
    pub calls: Arc<Mutex<Vec<Call>>>,
}

impl FakeMedia {
    pub fn start() -> Self {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind the fake comp-media");
        let port = listener.local_addr().expect("fake comp-media address").port();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let log = calls.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let log = log.clone();
                std::thread::spawn(move || serve(stream, &log));
            }
        });
        Self { port, calls }
    }
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    pub fn egress(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
    pub fn calls(&self, path: &str) -> Vec<Call> {
        self.calls
            .lock()
            .expect("the call log")
            .iter()
            .filter(|c| c.path == path)
            .cloned()
            .collect()
    }
}

/// One connection: every request on it, until the client closes it.
pub fn serve(stream: std::net::TcpStream, log: &Arc<Mutex<Vec<Call>>>) {
    let mut out = stream.try_clone().expect("clone the stream");
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        let (mut len, mut chunked, mut auth) = (0usize, false, String::new());
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                break;
            }
            let lower = h.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            } else if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
                chunked = true;
            } else if lower.starts_with("authorization:") {
                auth = h["authorization:".len()..].trim().to_string();
            }
        }
        let mut body = Vec::new();
        if chunked {
            loop {
                let mut size = String::new();
                if reader.read_line(&mut size).is_err() {
                    return;
                }
                let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
                let mut chunk = vec![0u8; n + 2];
                if reader.read_exact(&mut chunk).is_err() {
                    return;
                }
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
        } else if len > 0 {
            body.resize(len, 0);
            if reader.read_exact(&mut body).is_err() {
                return;
            }
        }
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        log.lock().expect("the call log").push(Call {
            path: path.clone(),
            auth: auth.clone(),
            body: body.clone(),
        });

        let (status, answer) = if auth != format!("Bearer {TOKEN}") {
            // What `daemon_auth` does with a missing or wrong token.
            ("401 Unauthorized", json!({"error": "unauthorized"}))
        } else {
            ("200 OK", answer_for(&path, &body))
        };
        let text = answer.to_string();
        let _ = write!(
            out,
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{text}",
            text.len()
        );
        let _ = out.flush();
    }
}

/// CONTRACT.md's answers, canned. Refusals are 200s with an `error`, as the
/// contract says, so the component's mapping of them is what is under test.
pub fn answer_for(path: &str, body: &Value) -> Value {
    let s = |k: &str| body[k].as_str().unwrap_or_default().to_string();
    match path {
        "/uploads" => {
            let id = s("photo_id");
            if id.is_empty()
                || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return json!({"error": "refused", "detail": "photo_id is not a key-safe id"});
            }
            let ct = s("content_type");
            let accepted =
                ["", "application/octet-stream", "image/jpeg", "image/x-sony-arw", "image/arw"];
            if !accepted.contains(&ct.as_str()) {
                return json!({"error": "refused", "detail": format!("content type {ct:?}")});
            }
            let size = body["size"].as_u64().unwrap_or(0);
            if size > MAX_BYTES {
                return json!({"error": "refused", "detail": "over the 512 MiB upload cap"});
            }
            let ext = s("filename").rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
            let key = format!("originals/{id}.{ext}");
            let n = size.div_ceil(PART).max(1);
            let parts: Vec<Value> = (1..=n)
                .map(|i| json!({"number": i, "url": format!("http://store.test/{key}?partNumber={i}&uploadId=up-{id}")}))
                .collect();
            json!({"upload_id": format!("up-{id}"), "key": key, "part_size": PART, "parts": parts, "expires_at": 4_102_444_800u64})
        }
        "/uploads/complete" => json!({"key": s("key")}),
        "/uploads/abort" => json!({}),
        "/jobs" => {
            let (job, photo) = (s("job_id"), s("photo_id"));
            if job.is_empty()
                || !job.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return json!({"error": "refused", "detail": "job_id is not a key-safe id"});
            }
            if !s("key").starts_with(&format!("originals/{photo}.")) {
                return json!({"error": "refused", "detail": "key is not this photo's original"});
            }
            let cb = s("callback_url");
            if !(cb.starts_with("http://") || cb.starts_with("https://")) {
                return json!({"error": "refused", "detail": "callback_url must be http(s)"});
            }
            json!({"job_id": job, "queued": true})
        }
        "/sign" => {
            json!({"url": format!("http://store.test/{}?X-Amz-Expires={}", s("key"), body["ttl_secs"])})
        }
        _ => json!({"error": "not-found", "detail": format!("no route {path}")}),
    }
}

/// `sha256=<hex HMAC-SHA256 of the body>` — the header the worker sends.
pub fn sign(body: &str, secret: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(body.as_bytes());
    let hex: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

pub fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}

/// A finished evaluation, shaped as CONTRACT.md's callback body.
pub fn result_for(id: &str) -> Value {
    json!({
        "job_id": id, "photo_id": id, "status": "done", "error": null,
        "backend": {"sharpness": "metal", "develop": "coreimage", "vision": true},
        "sha256": "ab".repeat(32),
        "metadata": {
            "camera": "Sony ILCE-7RM5", "lens": "FE 70-200mm F2.8 GM OSS II",
            "captured_at": "2026-09-23T13:11:47", "exposure_s": 0.004, "fnumber": 4.0,
            "focal_mm": 200.0, "iso": 6400, "width": 9504, "height": 6336
        },
        "renditions": {
            "thumb": {"key": format!("renditions/{id}/thumb.jpg"), "width": 512, "height": 341, "bytes": 41210},
            "share": {"key": format!("renditions/{id}/share.jpg"), "width": 4096, "height": 2731, "bytes": 2874394},
            "ai":    {"key": format!("renditions/{id}/ai.jpg"), "width": 1568, "height": 1045, "bytes": 323338}
        },
        "sharpness": {
            "method": "green-stab-v1", "grid": 16, "tiles": vec![128.7; 256],
            "floor": 128.7, "peak": 912.3, "focus_ratio": 6.1,
            "subjects": [{"kind": "face", "box": [0.23, 0.21, 0.18, 0.27], "sharpness": 59.1, "ratio": 0.5}]
        },
        "vision": {
            "labels": [{"id": "people", "confidence": 0.96}],
            "faces": [{"box": [0.23, 0.21, 0.18, 0.27], "quality": 0.55}],
            "attention": [[0.14, 0.11, 0.39, 0.77]], "objectness": [[0.0, 0.06, 1.0, 0.94]],
            "aesthetics": {"overall": 0.688, "utility": false}, "horizon_deg": -9.63
        },
        "colour": {"mean_luma": 0.41, "clipped_shadows_pct": 0.3, "clipped_highlights_pct": 0.1, "saturation_mean": 0.22},
        "timings_ms": {"download": 900, "decode": 15, "sharpness": 3, "develop": 1350, "vision": 550, "upload": 400}
    })
}


/// Compose photoquest and start it against a fresh fake `comp-media`, with the
/// four media config keys set plus `extra` (`key=value`, e.g. `allow-test-routes=true`).
/// `None` when the gate skips (no host / no build), like every gate here.
pub fn start(extra: &[&str]) -> Option<(Gate, FakeMedia)> {
    let media = FakeMedia::start();
    let media_url = format!("media-url={}", media.url());
    let token = format!("media-token={TOKEN}");
    let base = format!("public-callback-base={CALLBACK_BASE}");
    let secret = format!("media-callback-secret={SECRET}");
    let mut config = vec![media_url.as_str(), token.as_str(), base.as_str(), secret.as_str()];
    config.extend_from_slice(extra);
    let egress = media.egress();
    let gate = Gate::compose_and_start_with_egress("photoquest", CRATE, &config, &[&egress])?;
    Some((gate, media))
}
