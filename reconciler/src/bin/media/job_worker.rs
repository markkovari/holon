//! The worker: everything from "the original is in the bucket" to "here is
//! the result", and the pull loop that feeds it from `MEDIA_JOBS`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::apple_helper::run_helper;
use crate::decode::{decode_jpeg, decode_raw};
use crate::develop::{colour, develop_cpu, renditions_cpu, Rendition, RENDITIONS};
use crate::keys::{parse_key, valid_id, Which};
use crate::routes::{Daemon, Job};
use crate::sharpness::green_stab_v1;
use crate::store::{callback_allowed, hex};

const CALLBACK_ATTEMPTS: u32 = 5;

type HmacSha256 = Hmac<Sha256>;

/// `sha256=<hex>` over the raw body — the GitHub scheme `webhook:sign`
/// verifies with `scheme::github`.
fn signature(body: &[u8], secret: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
    mac.update(body);
    format!("sha256={}", hex(&mac.finalize().into_bytes()))
}

fn ms(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

/// Everything from "the original is in the bucket" to "here is the result".
async fn evaluate(d: &Daemon, job: &Job, dir: &Path) -> Result<Value> {
    let (_, object) = parse_key(&job.key).ok_or_else(|| anyhow!("bad key {}", job.key))?;
    let ext = object.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
    let original = dir.join(format!("original.{ext}"));
    let mut timings = std::collections::BTreeMap::new();

    let t = Instant::now();
    let (size, sha256) =
        d.store.download(Which::Originals, object, &original, d.max_upload).await?;
    timings.insert("download".to_string(), ms(t));
    eprintln!("comp-media: [{}] downloaded {size} bytes in {} ms", job.job_id, ms(t));

    let t = Instant::now();
    let (path, e) = (original.clone(), ext.clone());
    let mut dec = tokio::task::spawn_blocking(move || {
        if e == "arw" {
            decode_raw(&path)
        } else {
            decode_jpeg(&path)
        }
    })
    .await??;
    timings.insert("decode".to_string(), ms(t));
    eprintln!(
        "comp-media: [{}] decoded, green plane {}x{} in {} ms",
        job.job_id,
        dec.gw,
        dec.gh,
        ms(t)
    );

    // The Apple path first; any failure there falls back to the CPU path, so a
    // broken helper degrades the result instead of losing it.
    let mut helper_out = None;
    if let Some(helper) = d.apple() {
        let t = Instant::now();
        match run_helper(helper, &original, &dec, dir).await {
            Ok(h) => {
                eprintln!("comp-media: [{}] apple helper done in {} ms", job.job_id, ms(t));
                helper_out = Some(h);
            }
            Err(e) => eprintln!(
                "comp-media: [{}] apple helper failed ({e:#}); falling back to the CPU",
                job.job_id
            ),
        }
    }

    let mut renditions: Vec<Rendition> = Vec::new();
    let (develop, sharp_backend, sharpness, vision);
    match helper_out {
        Some(h) => {
            for (name, _) in RENDITIONS {
                let r = h
                    .renditions
                    .get(name)
                    .ok_or_else(|| anyhow!("helper wrote no {name} rendition"))?;
                let bytes = tokio::fs::read(dir.join(format!("{name}.jpg"))).await?;
                renditions.push((name, r.width, r.height, bytes));
            }
            develop = h.develop.unwrap_or_else(|| "coreimage".into());
            for (k, v) in h.timings_ms {
                timings.insert(k, v);
            }
            match h.sharpness {
                Some(s) => {
                    sharp_backend = "metal";
                    sharpness = s;
                }
                None => {
                    let t = Instant::now();
                    sharp_backend = "cpu";
                    sharpness = green_stab_v1(&dec.green, dec.gw, dec.gh);
                    timings.insert("sharpness".into(), ms(t));
                }
            }
            vision = h.vision.unwrap_or(Value::Null);
        }
        None => {
            let t = Instant::now();
            sharp_backend = "cpu";
            sharpness = green_stab_v1(&dec.green, dec.gw, dec.gh);
            timings.insert("sharpness".into(), ms(t));
            eprintln!("comp-media: [{}] sharpness (cpu) in {} ms", job.job_id, ms(t));

            let t = Instant::now();
            let (path, e) = (original.clone(), ext.clone());
            let (out, dev) = tokio::task::spawn_blocking(move || -> Result<_> {
                let (full, dev) = develop_cpu(&path, &e)?;
                Ok((renditions_cpu(&full)?, dev))
            })
            .await??;
            renditions = out;
            develop = dev.to_string();
            timings.insert("develop".into(), ms(t));
            eprintln!("comp-media: [{}] developed ({develop}) in {} ms", job.job_id, ms(t));
            vision = Value::Null;
        }
    }
    let metadata = std::mem::take(&mut dec.metadata);
    drop(dec);

    let ai = renditions.iter().find(|r| r.0 == "ai").map(|r| r.3.clone()).unwrap_or_default();
    let colour = tokio::task::spawn_blocking(move || colour(&ai)).await??;

    let t = Instant::now();
    let mut rendition_json = serde_json::Map::new();
    for (name, w, h, bytes) in renditions {
        let object = format!("{}/{name}.jpg", job.photo_id);
        let len = bytes.len();
        d.store.put(Which::Renditions, &object, bytes, "image/jpeg").await?;
        rendition_json.insert(
            name.to_string(),
            json!({ "key": format!("renditions/{object}"), "width": w, "height": h, "bytes": len }),
        );
    }
    timings.insert("upload".into(), ms(t));
    eprintln!("comp-media: [{}] renditions uploaded in {} ms", job.job_id, ms(t));

    Ok(json!({
        "job_id": job.job_id,
        "photo_id": job.photo_id,
        "status": "done",
        "error": null,
        "backend": { "sharpness": sharp_backend, "develop": develop, "vision": !vision.is_null() },
        "sha256": sha256,
        "metadata": metadata,
        "renditions": rendition_json,
        "sharpness": sharpness,
        "vision": vision,
        "colour": colour,
        "timings_ms": timings,
    }))
}

fn failed_body(job: &Job, error: &str) -> Value {
    json!({
        "job_id": job.job_id, "photo_id": job.photo_id, "status": "failed", "error": error,
        "backend": null, "sha256": null, "metadata": null, "renditions": null,
        "sharpness": null, "vision": null, "colour": null, "timings_ms": null,
    })
}

/// POST `body`, signed, retrying a non-2xx with backoff. True once accepted.
async fn deliver(
    http: &reqwest::Client,
    url: &str,
    body: &Value,
    secret: &str,
    attempts: u32,
) -> bool {
    let raw = serde_json::to_vec(body).unwrap_or_default();
    let sig = signature(&raw, secret);
    for attempt in 1..=attempts {
        let sent = http
            .post(url)
            .header("Content-Type", "application/json")
            .header("X-Media-Signature", &sig)
            .timeout(Duration::from_secs(30))
            .body(raw.clone())
            .send()
            .await;
        match sent {
            Ok(r) if r.status().is_success() => return true,
            Ok(r) => eprintln!(
                "comp-media: callback {url} answered {} (attempt {attempt}/{attempts})",
                r.status()
            ),
            Err(e) => {
                eprintln!("comp-media: callback {url} failed: {e} (attempt {attempt}/{attempts})")
            }
        }
        if attempt < attempts {
            tokio::time::sleep(Duration::from_secs(1 << (attempt - 1))).await;
        }
    }
    false
}

async fn run_job(d: &Daemon, job: &Job) {
    let started = Instant::now();
    let dir = d.work_dir.join(&job.job_id);
    let result = async {
        tokio::fs::create_dir_all(&dir).await?;
        evaluate(d, job, &dir).await
    }
    .await;
    // Always, however the job went.
    let _ = tokio::fs::remove_dir_all(&dir).await;

    let body = match result {
        Ok(v) => v,
        Err(e) => {
            eprintln!("comp-media: [{}] failed: {e:#}", job.job_id);
            failed_body(job, &format!("{e:#}"))
        }
    };
    let status = body["status"].as_str().unwrap_or("?").to_string();
    let ok =
        deliver(&d.store.http, &job.callback_url, &body, &d.callback_secret, CALLBACK_ATTEMPTS)
            .await;
    if !ok && status == "done" {
        // The contract: after the last attempt, a final `failed` — best effort,
        // to the same receiver, so a receiver that recovers learns the job
        // will not be retried.
        let fb =
            failed_body(job, &format!("callback not accepted after {CALLBACK_ATTEMPTS} attempts"));
        deliver(&d.store.http, &job.callback_url, &fb, &d.callback_secret, 1).await;
    }
    eprintln!(
        "comp-media: [{}] {status}, callback {} — {} ms total",
        job.job_id,
        if ok { "accepted" } else { "NOT accepted" },
        ms(started)
    );
}

pub(crate) async fn worker(
    d: Arc<Daemon>,
    consumer: async_nats::jetstream::consumer::PullConsumer,
) {
    use futures::StreamExt;
    loop {
        let batch =
            consumer.batch().max_messages(1).expires(Duration::from_secs(30)).messages().await;
        let mut batch = match batch {
            Ok(b) => b,
            Err(e) => {
                eprintln!("comp-media: pull failed: {e}; retrying");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        while let Some(msg) = batch.next().await {
            let msg = match msg {
                Ok(m) => Arc::new(m),
                Err(e) => {
                    eprintln!("comp-media: message error: {e}");
                    continue;
                }
            };
            let job: Job = match serde_json::from_slice(&msg.payload) {
                Ok(j) => j,
                Err(e) => {
                    // Not a job and never will be: ack it away rather than
                    // redeliver garbage forever.
                    eprintln!("comp-media: dropping an unreadable job: {e}");
                    let _ = msg.ack().await;
                    continue;
                }
            };
            // `/jobs` checked these, but anything with NATS access can publish
            // to the subject, and a job_id becomes a directory name.
            if !valid_id(&job.job_id)
                || !matches!(parse_key(&job.key), Some((Which::Originals, _)))
                || callback_allowed(&job.callback_url, &d.callback_allow).is_err()
            {
                eprintln!(
                    "comp-media: dropping a job with a bad id, key or callback: {:?} {:?} {:?}",
                    job.job_id, job.key, job.callback_url
                );
                let _ = msg.ack().await;
                continue;
            }
            eprintln!("comp-media: [{}] started ({})", job.job_id, job.key);
            let progress = {
                let msg = msg.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(20)).await;
                        let _ = msg.ack_with(async_nats::jetstream::AckKind::Progress).await;
                    }
                })
            };
            run_job(&d, &job).await;
            progress.abort();
            if let Err(e) = msg.double_ack().await {
                eprintln!("comp-media: [{}] ack failed: {e}", job.job_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The callback signature is `webhook:sign`'s GitHub scheme: `sha256=`
    /// and the lower-case hex HMAC-SHA256 of the raw body. The expected value
    /// is RFC 4231 test case 2 ("Jefe" / "what do ya want for nothing?"), so
    /// this pins the scheme, not just self-consistency.
    #[test]
    fn the_callback_signature_is_the_github_scheme() {
        assert_eq!(
            signature(b"what do ya want for nothing?", "Jefe"),
            "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
