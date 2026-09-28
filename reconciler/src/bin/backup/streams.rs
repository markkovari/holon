//! JetStream streams, dumped message by message and replayed.
//!
//! Every piece of state on NATS is a stream underneath: a KV bucket is `KV_<b>`,
//! an object store is `OBJ_<b>`, a queue is itself. So one mechanism backs up
//! all of them, history and delete markers included, without knowing which is
//! which: the stream's config, then each message's subject, headers and bytes,
//! in sequence order.
//!
//! ```text
//! {"stream":"KV_x","config":{...},"messages":N,"last_seq":S}   -- line 1
//! {"s":"$KV.x.k","h":[["KV-Operation","DEL"]],"d":"<base64>"}  -- one per message
//! ```
//!
//! **What a restore changes.** Sequence numbers restart from 1 and every
//! message is timestamped at the restore, so KV revisions differ from the
//! original and a `max_age` restarts. Anything that holds a revision across a
//! restore — a pending compare-and-set, a vcs intent guard — must re-read,
//! which is what they do on a mismatch anyway. Headers a publisher used to
//! guard its write (`Nats-Expected-*`) or deduplicate it (`Nats-Msg-Id`) are
//! dropped on replay: the stored message carries them, and replaying them
//! against a fresh stream would refuse every guarded write.

use std::io::{BufRead, Write};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_nats::jetstream::{self, consumer, stream};
use async_nats::HeaderMap;
use base64::Engine as _;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// How long a dump waits for the next message before deciding the stream has
/// nothing more — only reached if the server stops sending mid-stream.
const IDLE: Duration = Duration::from_secs(15);

#[derive(Serialize, Deserialize)]
struct Header {
    stream: String,
    config: stream::Config,
    messages: u64,
    last_seq: u64,
}

#[derive(Serialize, Deserialize)]
struct Line {
    s: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    h: Vec<(String, String)>,
    d: String,
}

pub struct Dumped {
    pub messages: u64,
}

/// Every stream name, filtered by `globs` (`*` matches any run of characters;
/// none given means all).
pub async fn names(js: &jetstream::Context, globs: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut names = js.stream_names();
    while let Some(n) = names.next().await {
        let n = n.context("listing streams")?;
        if globs.is_empty() || globs.iter().any(|g| glob(g, &n)) {
            out.push(n);
        }
    }
    out.sort();
    Ok(out)
}

pub fn glob(pattern: &str, s: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == s;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !s.starts_with(first) || !s[first.len()..].ends_with(last) {
        return false;
    }
    let mut rest = &s[first.len()..s.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// `None` for a stream that should not be backed up by itself — a mirror,
/// which restores itself from its source.
pub async fn dump(
    js: &jetstream::Context,
    name: &str,
    out: &mut impl Write,
) -> Result<Option<Dumped>> {
    let mut stream = js.get_stream(name).await.with_context(|| format!("stream {name}"))?;
    let info = stream.info().await.with_context(|| format!("stream {name} info"))?.clone();
    if info.config.mirror.is_some() {
        return Ok(None);
    }
    let (count, last) = (info.state.messages, info.state.last_sequence);
    let header = Header {
        stream: name.to_string(),
        config: info.config.clone(),
        messages: count,
        last_seq: last,
    };
    serde_json::to_writer(&mut *out, &header)?;
    out.write_all(b"\n")?;
    if count == 0 {
        return Ok(Some(Dumped { messages: 0 }));
    }

    let mut written = 0u64;
    let ordered = stream
        .create_consumer(consumer::pull::OrderedConfig {
            deliver_policy: consumer::DeliverPolicy::All,
            ..Default::default()
        })
        .await;
    match ordered {
        Ok(c) => {
            let mut msgs = c.messages().await.with_context(|| format!("reading {name}"))?;
            loop {
                let next = match tokio::time::timeout(IDLE, msgs.next()).await {
                    Ok(Some(m)) => m.with_context(|| format!("reading {name}"))?,
                    Ok(None) => break,
                    Err(_) => {
                        eprintln!(
                            "comp-backup: {name}: no message for {IDLE:?}; taking what arrived"
                        );
                        break;
                    }
                };
                let (seq, pending) = {
                    let i = next.info().map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
                    (i.stream_sequence, i.pending)
                };
                write_line(out, next.subject.as_str(), next.headers.as_ref(), &next.payload)?;
                written += 1;
                // Caught up with what the stream held when this message was
                // delivered. Later writes belong to the next backup.
                if pending == 0 || seq >= last {
                    break;
                }
            }
        }
        // A work-queue stream refuses a second, unfiltered consumer. Read it by
        // sequence instead: slower, one request per message, and it works.
        Err(e) => {
            eprintln!("comp-backup: {name}: no ordered consumer ({e}); reading by sequence");
            for seq in info.state.first_sequence..=last {
                match stream.get_raw_message(seq).await {
                    Ok(m) => {
                        write_line(out, m.subject.as_str(), Some(&m.headers), &m.payload)?;
                        written += 1;
                    }
                    Err(e) if e.kind() == stream::RawMessageErrorKind::NoMessageFound => {}
                    Err(e) => return Err(anyhow::anyhow!("{name} seq {seq}: {e}")),
                }
            }
        }
    }
    Ok(Some(Dumped { messages: written }))
}

fn write_line(
    out: &mut impl Write,
    subject: &str,
    headers: Option<&HeaderMap>,
    payload: &[u8],
) -> Result<()> {
    let mut h = Vec::new();
    if let Some(map) = headers {
        for (k, vs) in map.iter() {
            for v in vs {
                h.push((k.to_string(), v.to_string()));
            }
        }
        h.sort();
    }
    serde_json::to_writer(&mut *out, &Line { s: subject.to_string(), h, d: B64.encode(payload) })?;
    out.write_all(b"\n")?;
    Ok(())
}

fn replayable(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    !(n.starts_with("nats-expected-") || n == "nats-msg-id")
}

pub struct Restored {
    pub stream: String,
    pub messages: u64,
}

/// Replay one dumped stream. An existing stream of that name is refused
/// unless `replace`, which deletes it first. `replicas` overrides the saved
/// replica count, for a restore onto a smaller cluster than the backup's.
pub async fn restore(
    js: &jetstream::Context,
    input: impl BufRead,
    replace: bool,
    replicas: Option<usize>,
) -> Result<Restored> {
    let mut lines = input.lines();
    let first = lines.next().context("an empty stream dump")??;
    let header: Header = serde_json::from_str(&first).context("the dump's header line")?;
    let mut cfg = header.config;
    if let Some(r) = replicas {
        cfg.num_replicas = r;
    }
    if cfg.sealed {
        eprintln!("comp-backup: {} was sealed; restored unsealed", cfg.name);
        cfg.sealed = false;
    }
    // Server-maintained metadata (`_nats.*`) is the server's to set.
    cfg.metadata.retain(|k, _| !k.starts_with("_nats"));
    if js.get_stream(&cfg.name).await.is_ok() {
        if !replace {
            bail!(
                "stream {} already exists — restore into an empty server, or pass --replace",
                cfg.name
            );
        }
        js.delete_stream(&cfg.name).await.with_context(|| format!("deleting {}", cfg.name))?;
    }
    js.create_stream(cfg.clone()).await.with_context(|| format!("creating {}", cfg.name))?;

    let mut acks = Vec::new();
    let mut n = 0u64;
    for line in lines {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let l: Line = serde_json::from_str(&line).context("a dumped message")?;
        let payload = B64.decode(&l.d).context("a dumped payload")?;
        let mut headers = HeaderMap::new();
        for (k, v) in l.h.iter().filter(|(k, _)| replayable(k)) {
            headers.append(k.as_str(), v.as_str());
        }
        let ack = js
            .publish_with_headers(l.s, headers, payload.into())
            .await
            .with_context(|| format!("replaying into {}", cfg.name))?;
        acks.push(ack);
        n += 1;
        // Pipelined, in order on one connection; bounded so a large stream
        // does not queue unboundedly many acks.
        if acks.len() >= 256 {
            for a in acks.drain(..) {
                a.await.with_context(|| format!("replaying into {}", cfg.name))?;
            }
        }
    }
    for a in acks {
        a.await.with_context(|| format!("replaying into {}", cfg.name))?;
    }
    Ok(Restored { stream: cfg.name, messages: n })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_the_way_a_shell_would() {
        assert!(glob("KV_*", "KV_b-acme"));
        assert!(!glob("KV_*", "OBJ_x"));
        assert!(glob("*", "anything"));
        assert!(glob("OBJ_*_blobs", "OBJ_vcs_blobs"));
        assert!(!glob("OBJ_*_blobs", "OBJ_vcs_blob"));
        assert!(glob("MEDIA_JOBS", "MEDIA_JOBS"));
        assert!(glob("a*b*c", "a-x-b-y-c"));
        assert!(!glob("a*b*c", "a-x-c"));
    }

    #[test]
    fn guard_headers_are_dropped_and_data_headers_kept() {
        assert!(!replayable("Nats-Expected-Last-Subject-Sequence"));
        assert!(!replayable("nats-msg-id"));
        assert!(replayable("KV-Operation"));
        assert!(replayable("Nats-Rollup"));
    }
}
