//! `photoquest` — a photo is planned, completed, queued and evaluated, and only its
//! owner ever sees it.
//!
//! ## A fake comp-media, on purpose
//!
//! The real daemon needs an S3 store and a NATS server and, for the interesting half
//! of an evaluation, a Mac. None of that is in CI and none of it is what this gate
//! judges: the component's whole job is deciding who may upload what, turning that
//! into the right calls to `comp-media`, and refusing a result it cannot verify
//! (ADR-0098). So the gate runs its own `comp-media` — a recording HTTP server on a
//! port the OS picks, answering `components/media-pipeline/CONTRACT.md`'s routes with
//! canned plans — and asserts on what ARRIVED there, not only on what the component
//! said back. A component that answered `processing` without ever calling `/jobs`
//! passes every check that reads its own replies.
//!
//! The callback is sent by the gate itself, signed the way the contract says the
//! worker signs it (GitHub-style HMAC-SHA256 over the raw body), which is the only
//! way to also send one signed WRONGLY.

mod gatelib;
use gatelib::{field, Gate};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

const CRATE: &str = "photoquest-domain";
const SECRET: &str = "gate-callback-secret";
const TOKEN: &str = "gate-media-token";
const CALLBACK_BASE: &str = "http://photoquest.test";
/// Mirrors the daemon's `--max-upload-mb` default, so a refusal can be arranged.
const MAX_BYTES: u64 = 512 * 1024 * 1024;
const PART: u64 = 16 * 1024 * 1024;

/// One request the fake daemon received.
#[derive(Clone, Debug)]
struct Call {
    path: String,
    auth: String,
    body: Value,
}

/// `comp-media`, minus the store, the queue and the GPU.
struct FakeMedia {
    port: u16,
    calls: Arc<Mutex<Vec<Call>>>,
}

impl FakeMedia {
    fn start() -> Self {
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
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn egress(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
    fn calls(&self, path: &str) -> Vec<Call> {
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
fn serve(stream: std::net::TcpStream, log: &Arc<Mutex<Vec<Call>>>) {
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
fn answer_for(path: &str, body: &Value) -> Value {
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
fn sign(body: &str, secret: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(body.as_bytes());
    let hex: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}

/// A finished evaluation, shaped as CONTRACT.md's callback body.
fn result_for(id: &str) -> Value {
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

#[test]
fn a_photo_is_planned_completed_evaluated_and_only_its_owner_sees_it() {
    let media = FakeMedia::start();
    let media_url = format!("media-url={}", media.url());
    let token = format!("media-token={TOKEN}");
    let base = format!("public-callback-base={CALLBACK_BASE}");
    let secret = format!("media-callback-secret={SECRET}");
    let config = [media_url.as_str(), token.as_str(), base.as_str(), secret.as_str()];
    let egress = media.egress();
    let Some(gate) = Gate::compose_and_start_with_egress("photoquest", CRATE, &config, &[&egress])
    else {
        return;
    };

    gatelib::requires_capability(
        CRATE,
        "media:pipeline/jobs",
        "uploads and evaluation go through media:pipeline — a component that talked to the \
         store or the queue itself would be holding credentials ADR-0098 keeps in comp-media",
    );
    gatelib::requires_capability(
        CRATE,
        "webhook:sign/signer",
        "the callback is verified with webhook:sign — a hand-rolled HMAC compare is how a \
         timing leak or a non-constant-time check gets in",
    );
    gatelib::requires_capability(
        CRATE,
        "auth:identity/authorizer",
        "who is calling is auth:identity's answer, not something this component parses",
    );

    gatelib::assert_unauthenticated(&gate, "GET", "/api/photos", None);
    gatelib::assert_unauthenticated(
        &gate,
        "POST",
        "/api/photos",
        Some(json!({"filename": "a.arw", "size": 1, "content_type": "image/x-sony-arw"})),
    );

    // --- two real accounts ---------------------------------------------------
    let run = std::process::id();
    let login = |who: &str| -> String {
        let email = format!("{who}-{run}@photoquest.test");
        let (code, reg) =
            gate.post("/register", None, json!({"email": email, "password": "correct horse"}));
        assert_eq!(code, 201, "register {who} failed: {reg}");
        let (code, out) =
            gate.post("/login", None, json!({"email": email, "password": "correct horse"}));
        assert_eq!(code, 200, "login {who} failed: {out}");
        field(&out, "access_token")
    };
    let (ada, bob) = (login("ada"), login("bob"));
    assert!(!ada.is_empty() && !bob.is_empty(), "login did not hand back access tokens");

    // --- planning an upload --------------------------------------------------
    let size: u64 = 40 * 1024 * 1024;
    let (code, out) = gate.post(
        "/api/photos",
        Some(&ada),
        json!({"filename": "DSC01234.ARW", "size": size, "content_type": "image/x-sony-arw"}),
    );
    assert_eq!(code, 201, "POST /api/photos must answer 201 with a plan: {out}");
    let created = parse(&out);
    let id = created["photo"]["id"].as_str().unwrap_or_default().to_string();
    assert!(!id.is_empty(), "the created photo has no id: {out}");
    assert_eq!(created["photo"]["state"], "uploading", "a new photo is uploading: {out}");
    assert_eq!(
        created["upload"]["parts"].as_array().map(Vec::len),
        Some(3),
        "40 MiB in 16 MiB parts is three: {out}"
    );
    assert_eq!(created["upload"]["key"], format!("originals/{id}.arw"), "the plan's key: {out}");

    let uploads = media.calls("/uploads");
    assert_eq!(uploads.len(), 1, "exactly one /uploads call for one photo, got {uploads:?}");
    let u = &uploads[0];
    assert_eq!(
        u.body["photo_id"], id,
        "start-upload must be asked with the photo's own id: {:?}",
        u.body
    );
    assert_eq!(u.body["size"], size, "the size was not forwarded: {:?}", u.body);
    assert_eq!(
        u.body["content_type"], "image/x-sony-arw",
        "the content type was not forwarded: {:?}",
        u.body
    );
    assert_eq!(u.body["filename"], "DSC01234.ARW", "the filename was not forwarded: {:?}", u.body);
    assert_eq!(u.auth, format!("Bearer {TOKEN}"), "media-token must reach the daemon as a bearer");

    // A refusal is the daemon's decision, and the caller must be able to read it —
    // and must not leave a photo behind that can never be uploaded.
    let (code, out) = gate.post(
        "/api/photos",
        Some(&ada),
        json!({"filename": "huge.ARW", "size": MAX_BYTES + 1, "content_type": "image/x-sony-arw"}),
    );
    assert_eq!(code, 422, "an upload the daemon refuses is 422, not {code}: {out}");
    assert_eq!(field(&out, "error"), "media_refused", "the refusal must say what it is: {out}");
    let (_, list) = gate.get("/api/photos", Some(&ada));
    assert_eq!(
        parse(&list)["photos"].as_array().map(Vec::len),
        Some(1),
        "a refused upload left a photo in the gallery: {list}"
    );

    // --- somebody else's photo -----------------------------------------------
    let (code, _) = gate.get(&format!("/api/photos/{id}"), Some(&bob));
    assert_eq!(code, 403, "bob read ada's photo");
    let parts = json!({"parts": [
        {"number": 1, "etag": "\"e1\""}, {"number": 2, "etag": "\"e2\""}, {"number": 3, "etag": "\"e3\""}
    ]});
    let (code, _) = gate.post(&format!("/api/photos/{id}/complete"), Some(&bob), parts.clone());
    assert_eq!(code, 403, "bob completed ada's upload");
    assert!(
        media.calls("/uploads/complete").is_empty(),
        "a refused complete still reached the daemon"
    );
    let (_, bobs) = gate.get("/api/photos", Some(&bob));
    assert_eq!(
        parse(&bobs)["photos"].as_array().map(Vec::len),
        Some(0),
        "bob's gallery shows ada's photo: {bobs}"
    );

    // A result for a photo that was never submitted is not written.
    let early = result_for(&id);
    let (code, out) = gate.with_headers(
        "POST",
        &format!("/internal/photos/{id}/evaluated"),
        None,
        &[("x-media-signature", &sign(&early.to_string(), SECRET))],
        Some(early.clone()),
    );
    assert_eq!(code, 409, "a callback before the upload was completed must be refused: {out}");

    // --- completing ----------------------------------------------------------
    let (code, out) = gate.post(&format!("/api/photos/{id}/complete"), Some(&ada), parts.clone());
    assert_eq!(code, 200, "complete must succeed for the owner: {out}");
    assert_eq!(field(&out, "state"), "processing", "a completed photo is processing: {out}");

    let completes = media.calls("/uploads/complete");
    assert_eq!(completes.len(), 1, "one complete-upload, got {completes:?}");
    assert_eq!(
        completes[0].body["upload_id"],
        format!("up-{id}"),
        "the plan's upload id: {:?}",
        completes[0].body
    );
    assert_eq!(
        completes[0].body["key"],
        format!("originals/{id}.arw"),
        "the plan's key: {:?}",
        completes[0].body
    );
    assert_eq!(
        completes[0].body["parts"][2]["etag"], "\"e3\"",
        "the ETags must reach the store verbatim: {:?}",
        completes[0].body
    );

    let jobs = media.calls("/jobs");
    assert_eq!(jobs.len(), 1, "one submit, got {jobs:?}");
    assert_eq!(
        jobs[0].body["job_id"], id,
        "job_id must be the photo id, so a resubmit dedupes: {:?}",
        jobs[0].body
    );
    assert_eq!(jobs[0].body["photo_id"], id, "{:?}", jobs[0].body);
    assert_eq!(jobs[0].body["key"], format!("originals/{id}.arw"), "{:?}", jobs[0].body);
    assert_eq!(
        jobs[0].body["callback_url"],
        format!("{CALLBACK_BASE}/internal/photos/{id}/evaluated"),
        "the callback URL must be public-callback-base plus the photo's route"
    );

    // A retried complete does not complete twice (the store would refuse it), and
    // resubmits under the SAME job id.
    let (code, out) = gate.post(&format!("/api/photos/{id}/complete"), Some(&ada), parts.clone());
    assert_eq!(code, 200, "a retried complete must still succeed: {out}");
    assert_eq!(
        media.calls("/uploads/complete").len(),
        1,
        "a retried complete completed the upload again"
    );
    let jobs = media.calls("/jobs");
    assert!(
        jobs.iter().all(|j| j.body["job_id"] == id),
        "a resubmit used a different job id: {jobs:?}"
    );

    let (_, got) = gate.get(&format!("/api/photos/{id}"), Some(&ada));
    assert_eq!(field(&got, "state"), "processing", "{got}");
    assert!(parse(&got)["urls"]["thumb"].is_null(), "no thumbnail URL before evaluation: {got}");

    // --- the callback ----------------------------------------------------------
    let result = result_for(&id);
    let raw = result.to_string();
    let path = format!("/internal/photos/{id}/evaluated");

    let (code, _) = gate.with_headers("POST", &path, None, &[], Some(result.clone()));
    assert_eq!(code, 401, "an unsigned callback must be refused");
    let (code, _) = gate.with_headers(
        "POST",
        &path,
        None,
        &[("x-media-signature", &sign(&raw, "wrong-secret"))],
        Some(result.clone()),
    );
    assert_eq!(code, 401, "a callback signed with the wrong secret must be refused");
    let mut tampered = result.clone();
    tampered["metadata"]["camera"] = json!("Forged Cam");
    let (code, _) = gate.with_headers(
        "POST",
        &path,
        None,
        &[("x-media-signature", &sign(&raw, SECRET))],
        Some(tampered),
    );
    assert_eq!(code, 401, "a body that is not the one signed must be refused");
    let (_, got) = gate.get(&format!("/api/photos/{id}"), Some(&ada));
    assert_eq!(field(&got, "state"), "processing", "a refused callback changed the photo: {got}");

    let (code, out) = gate.with_headers(
        "POST",
        &path,
        None,
        &[("x-media-signature", &sign(&raw, SECRET))],
        Some(result.clone()),
    );
    assert_eq!(code, 200, "a correctly signed callback must be accepted: {out}");
    assert_eq!(field(&out, "state"), "evaluated", "{out}");

    let (code, got) = gate.get(&format!("/api/photos/{id}"), Some(&ada));
    assert_eq!(code, 200, "{got}");
    let photo = parse(&got);
    assert_eq!(photo["state"], "evaluated", "{got}");
    assert_eq!(photo["metadata"]["camera"], "Sony ILCE-7RM5", "metadata was not stored: {got}");
    assert_eq!(photo["sharpness"]["focus_ratio"], 6.1, "sharpness was not stored: {got}");
    assert_eq!(
        photo["sharpness"]["subjects"][0]["sharpness"], 59.1,
        "per-face sharpness was not stored: {got}"
    );
    assert_eq!(photo["vision"]["labels"][0]["id"], "people", "vision was not stored: {got}");
    assert_eq!(photo["colour"]["mean_luma"], 0.41, "colour was not stored: {got}");
    assert_eq!(photo["backend"]["sharpness"], "metal", "backend was not stored: {got}");
    assert_eq!(photo["sha256"], "ab".repeat(32), "sha256 was not stored: {got}");
    for r in ["thumb", "share", "ai"] {
        let url = photo["urls"][r].as_str().unwrap_or_default();
        assert!(
            url.contains(&format!("renditions/{id}/{r}.jpg")),
            "GET must carry a signed {r} URL for an evaluated photo, got {url:?}"
        );
    }
    assert!(
        media.calls("/sign").iter().all(|c| c.body["ttl_secs"] == 3600),
        "renditions are signed for an hour: {:?}",
        media.calls("/sign")
    );
    let (code, _) = gate.get(&format!("/api/photos/{id}"), Some(&bob));
    assert_eq!(code, 403, "bob read ada's evaluated photo");

    // --- the same callback again, and a late failure -----------------------------
    let evaluated_at = photo["evaluated_at"].clone();
    let (code, out) = gate.with_headers(
        "POST",
        &path,
        None,
        &[("x-media-signature", &sign(&raw, SECRET))],
        Some(result.clone()),
    );
    assert_eq!(
        code, 200,
        "a repeated callback must still be acknowledged, or the worker retries forever: {out}"
    );
    assert_eq!(field(&out, "state"), "evaluated", "{out}");
    // A failure is every field null but these four (CONTRACT.md).
    let mut failed = json!({
        "job_id": id, "photo_id": id, "status": "failed", "error": "decode failed",
        "backend": null, "sha256": null, "metadata": null, "renditions": null,
        "sharpness": null, "vision": null, "colour": null, "timings_ms": null
    });
    let fraw = failed.to_string();
    let (code, _) = gate.with_headers(
        "POST",
        &path,
        None,
        &[("x-media-signature", &sign(&fraw, SECRET))],
        Some(failed.clone()),
    );
    assert_eq!(code, 200, "a late failure is acknowledged");
    let (_, again) = gate.get(&format!("/api/photos/{id}"), Some(&ada));
    let again = parse(&again);
    assert_eq!(again["state"], "evaluated", "a late failure must not undo an evaluation: {again}");
    assert_eq!(again["evaluated_at"], evaluated_at, "a repeated callback rewrote the photo");

    // A result whose body names another photo than its path.
    failed["photo_id"] = json!("someone-else");
    let fraw = failed.to_string();
    let (code, _) = gate.with_headers(
        "POST",
        &path,
        None,
        &[("x-media-signature", &sign(&fraw, SECRET))],
        Some(failed),
    );
    assert_eq!(code, 400, "a callback whose photo_id is not its path's must be refused");

    // --- the gallery -------------------------------------------------------------
    let (code, list) = gate.get("/api/photos", Some(&ada));
    assert_eq!(code, 200, "{list}");
    let photos = parse(&list)["photos"].clone();
    assert_eq!(photos.as_array().map(Vec::len), Some(1), "ada has one photo: {list}");
    assert_eq!(photos[0]["id"], id, "{list}");
    assert!(
        photos[0]["thumb_url"]
            .as_str()
            .unwrap_or_default()
            .contains(&format!("renditions/{id}/thumb.jpg")),
        "the gallery must carry a signed thumbnail for an evaluated photo: {list}"
    );
    assert!(
        photos[0]["sharpness"]["tiles"].is_null(),
        "the gallery should not ship 256 tiles per photo: {list}"
    );

    // --- a JPEG whose evaluation fails ---------------------------------------------
    //
    // Content type "" — what a browser reports for a file it does not recognise — is
    // passed through as it arrived, not replaced with a guess.
    let (code, out) = gate.post(
        "/api/photos",
        Some(&ada),
        json!({"filename": "scan.jpg", "size": 1_000_000, "content_type": ""}),
    );
    assert_eq!(code, 201, "{out}");
    let second = parse(&out)["photo"]["id"].as_str().unwrap_or_default().to_string();
    let last = media.calls("/uploads").pop().expect("a second /uploads call");
    assert_eq!(last.body["content_type"], "", "an empty content type must reach the daemon as-is");
    let one = json!({"parts": [{"number": 1, "etag": "\"j1\""}]});
    let (code, out) = gate.post(&format!("/api/photos/{second}/complete"), Some(&ada), one);
    assert_eq!(code, 200, "{out}");
    let failure = json!({
        "job_id": second, "photo_id": second, "status": "failed", "error": "decode failed",
        "backend": null, "sha256": null, "metadata": null, "renditions": null,
        "sharpness": null, "vision": null, "colour": null, "timings_ms": null
    });
    let (code, out) = gate.with_headers(
        "POST",
        &format!("/internal/photos/{second}/evaluated"),
        None,
        &[("x-media-signature", &sign(&failure.to_string(), SECRET))],
        Some(failure),
    );
    assert_eq!(code, 200, "a failure callback must be acknowledged: {out}");
    assert_eq!(field(&out, "state"), "failed", "{out}");
    let (code, got) = gate.get(&format!("/api/photos/{second}"), Some(&ada));
    assert_eq!(code, 200, "a failed photo must still be readable: {got}");
    let got = parse(&got);
    assert_eq!(got["state"], "failed", "{got}");
    assert_eq!(got["error"], "decode failed", "the failure's reason must be kept: {got}");
    assert!(got["urls"]["thumb"].is_null(), "a failed photo has no rendition to sign: {got}");

    let (_, list) = gate.get("/api/photos", Some(&ada));
    let photos = parse(&list)["photos"].clone();
    assert_eq!(photos.as_array().map(Vec::len), Some(2), "ada has two photos: {list}");
    assert_eq!(photos[0]["id"], second, "the gallery is newest first: {list}");
    assert!(photos[0]["thumb_url"].is_null(), "no thumbnail for a failed photo: {list}");
}
