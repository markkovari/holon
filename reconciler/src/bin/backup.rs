//! `comp-backup` — holon's state, copied off the machines that hold it.
//!
//! One copy per machine is not a backup (ADR-0067): `--kv-replicas 3` survives a
//! dead server, not a deleted bucket, a bad deploy, or losing the site. This
//! copies what the platform keeps to an S3-compatible bucket somewhere else —
//! Cloudflare R2, S3, a MinIO on another box — and puts it back.
//!
//! ## What is in a backup
//!
//! - **Every JetStream stream** (or those `--stream` names): host KV buckets
//!   (`KV_*`), object stores (`OBJ_*` — vcs blobs), queues. History and delete
//!   markers included ([`streams`]).
//! - **SurrealDB databases** named by `--surreal-db ns/db` — the vcs graph, the
//!   capability graph — as the server's own export ([`surreal`]).
//!
//! Not included: `--kv sqlite` files (node-local by design; back up the file),
//! and objects already in an S3 bucket via `--blob s3` (that bucket is the
//! durable copy — replicate it with the provider's own tooling).
//!
//! ## Layout
//!
//! ```text
//! <prefix>/<id>/streams/<stream>.jsonl.gz
//! <prefix>/<id>/surreal/<ns>.<db>.surql.gz
//! <prefix>/<id>/manifest.json            -- written LAST: the commit point
//! ```
//!
//! A backup without a manifest is an interrupted one: `list` shows it as such,
//! `restore` refuses it, and retention removes it once a later one completes.
//! The manifest records every part's size and SHA-256 and whether it is sealed.
//!
//! ## Sealed by default
//!
//! A backup holds every tenant's state, `secrets-vault` included, and is going
//! to someone else's bucket. So each part is sealed with `--encrypt-key-file`
//! ([`seal`]: ChaCha20-Poly1305, chunked, bound to its path in the backup), and
//! running without a key needs `--plaintext` said out loud. Lose the key and the
//! backups are noise; keep it somewhere that is not the bucket.
//!
//! ## Consistency
//!
//! Each stream is read up to where it stood when its read began, one stream
//! after another — not one instant across all of them. A store that spans two
//! (vcs: NATS pointers + SurrealDB graph) is written to be repaired from either
//! half lagging the other; run its `verify`/`repair` after a restore.
//!
//! ```text
//! comp-backup keygen > backup.key
//! comp-backup run --nats-url nats://127.0.0.1:4222 --encrypt-key-file backup.key \
//!   --s3-endpoint https://<acct>.r2.cloudflarestorage.com --s3-region auto \
//!   --s3-access-key-file r2.id --s3-secret-key-file r2.secret --keep 14 --every 6h
//! comp-backup list    --s3-endpoint ...
//! comp-backup restore --id latest --nats-url nats://new:4222 --encrypt-key-file backup.key ...
//! ```

#[path = "backup/seal.rs"]
mod seal;
#[path = "backup/store.rs"]
mod store;
#[path = "backup/streams.rs"]
mod streams;
#[path = "backup/surreal.rs"]
mod surreal;

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use seal::Key;
use store::{S3Args, Store};
use surreal::{Surreal, SurrealArgs};

#[derive(Parser)]
#[command(
    name = "comp-backup",
    about = "Back up holon's JetStream and SurrealDB state to S3/R2, and restore it"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Take a backup (once, or every `--every`).
    Run(RunArgs),
    /// The backups in the bucket, newest last.
    List {
        #[command(flatten)]
        s3: S3Args,
    },
    /// Put a backup back.
    Restore(RestoreArgs),
    /// Print a fresh 32-byte key, hex, for `--encrypt-key-file`.
    Keygen,
}

#[derive(clap::Args)]
struct NatsArgs {
    /// NATS URL, or a comma-separated list. Omit to back up SurrealDB only.
    #[arg(long)]
    nats_url: Option<String>,
    /// A `.creds` file for a NATS that requires one.
    #[arg(long)]
    nats_creds: Option<String>,
}

#[derive(clap::Args)]
struct RunArgs {
    #[command(flatten)]
    s3: S3Args,
    #[command(flatten)]
    nats: NatsArgs,
    #[command(flatten)]
    surreal: SurrealArgs,
    /// Stream names to include, `*` wildcards, repeatable. Default: all.
    #[arg(long = "stream")]
    streams: Vec<String>,
    #[arg(long)]
    encrypt_key_file: Option<String>,
    /// Write parts unsealed. Needed, explicitly, to run without a key.
    #[arg(long)]
    plaintext: bool,
    /// Keep this many complete backups; older ones are deleted. 0 keeps all.
    #[arg(long, default_value = "14")]
    keep: usize,
    /// Run forever, one backup per interval (`90s`, `30m`, `6h`, `1d`).
    #[arg(long, value_parser = parse_interval)]
    every: Option<Duration>,
}

#[derive(clap::Args)]
struct RestoreArgs {
    #[command(flatten)]
    s3: S3Args,
    #[command(flatten)]
    nats: NatsArgs,
    #[command(flatten)]
    surreal: SurrealArgs,
    /// A backup id from `list`, or `latest`.
    #[arg(long, default_value = "latest")]
    id: String,
    /// Restore only parts whose name matches, `*` wildcards, repeatable.
    #[arg(long = "only")]
    only: Vec<String>,
    #[arg(long)]
    encrypt_key_file: Option<String>,
    /// Delete an existing stream of the same name first. Without it an
    /// existing stream is refused — a restore does not merge.
    #[arg(long)]
    replace: bool,
    /// Override each stream's saved replica count (a smaller cluster).
    #[arg(long)]
    replicas: Option<usize>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Manifest {
    format: u32,
    id: String,
    created: String,
    sealed: bool,
    /// First 8 bytes of SHA-256(key); empty when not sealed.
    key_fingerprint: String,
    parts: Vec<Part>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Part {
    /// `stream` or `surreal`.
    kind: String,
    /// The stream name, or `ns/db`.
    name: String,
    /// Relative to the backup: `streams/KV_x.jsonl.gz`.
    path: String,
    bytes: u64,
    sha256: String,
    #[serde(default)]
    messages: u64,
}

fn parse_interval(s: &str) -> Result<Duration, String> {
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num.parse().map_err(|_| format!("{s}: expected e.g. 90s, 30m, 6h, 1d"))?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => return Err(format!("{s}: expected e.g. 90s, 30m, 6h, 1d")),
    };
    if secs == 0 {
        return Err("an interval of zero".into());
    }
    Ok(Duration::from_secs(secs))
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Keygen => {
            println!("{}", Key::generate()?);
            Ok(())
        }
        Cmd::List { s3 } => list(&s3).await,
        Cmd::Restore(a) => restore(a).await,
        Cmd::Run(a) => {
            let key = match (&a.encrypt_key_file, a.plaintext) {
                (Some(p), _) => Some(Key::load(p)?),
                (None, true) => None,
                (None, false) => bail!(
                    "no --encrypt-key-file: a backup holds every tenant's state and is going to \
                     someone else's bucket. Make a key with `comp-backup keygen`, or pass \
                     --plaintext if you mean it."
                ),
            };
            let Some(every) = a.every else {
                return run(&a, key.as_ref()).await.map(|_| ());
            };
            loop {
                // A daemon outlives a failed run: the next one may succeed.
                if let Err(e) = run(&a, key.as_ref()).await {
                    eprintln!("comp-backup: backup failed: {e:#}");
                }
                tokio::time::sleep(every).await;
            }
        }
    }
}

async fn nats(a: &NatsArgs) -> Result<Option<async_nats::jetstream::Context>> {
    let Some(url) = &a.nats_url else {
        return Ok(None);
    };
    let mut opts = async_nats::ConnectOptions::new();
    if let Some(c) = &a.nats_creds {
        opts = opts.credentials_file(c).await.with_context(|| format!("reading {c}"))?;
    }
    let servers: Vec<String> = url.split(',').map(|s| s.trim().to_string()).collect();
    let client =
        opts.connect(servers).await.with_context(|| format!("connecting to NATS at {url}"))?;
    Ok(Some(async_nats::jetstream::new(client)))
}

fn backup_id() -> Result<String> {
    let mut r = [0u8; 2];
    getrandom::getrandom(&mut r)?;
    // Sorts by time; the suffix keeps two runs in one second apart.
    Ok(format!("{}-{}", jiff::Timestamp::now().strftime("%Y%m%dT%H%M%SZ"), hex::encode(r)))
}

struct Hashing<W> {
    inner: W,
    hash: Sha256,
    n: u64,
}
impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        self.n += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Turn a gzip'd file into the bytes that are uploaded — sealed under `name`
/// if there is a key — and return (path, size, sha256).
fn finalize(raw: &Path, key: Option<&Key>, name: &str) -> Result<(PathBuf, u64, String)> {
    let out = raw.with_extension("part");
    let mut w =
        Hashing { inner: BufWriter::new(std::fs::File::create(&out)?), hash: Sha256::new(), n: 0 };
    let input = BufReader::new(std::fs::File::open(raw)?);
    match key {
        Some(k) => seal::seal(k, name, input, &mut w)?,
        None => {
            std::io::copy(&mut { input }, &mut w)?;
        }
    }
    w.flush()?;
    Ok((out, w.n, hex::encode(w.hash.finalize())))
}

async fn run(a: &RunArgs, key: Option<&Key>) -> Result<Manifest> {
    let store = Store::connect(&a.s3).await?;
    let js = nats(&a.nats).await?;
    let db = Surreal::new(&a.surreal)?;
    if js.is_none() && db.is_none() {
        bail!("nothing to back up: give --nats-url, --surreal-url, or both");
    }
    let id = backup_id()?;
    let base = format!("{}/{id}", a.s3.s3_prefix.trim_end_matches('/'));
    let scratch = tempfile::Builder::new().prefix(&format!("comp-backup-{id}-")).tempdir()?;
    let started = std::time::Instant::now();
    let mut parts = Vec::new();

    if let Some(js) = &js {
        for name in streams::names(js, &a.streams).await? {
            let raw = scratch.path().join(format!("{name}.jsonl.gz"));
            let mut gz = GzEncoder::new(
                BufWriter::new(std::fs::File::create(&raw)?),
                flate2::Compression::default(),
            );
            let Some(dumped) = streams::dump(js, &name, &mut gz).await? else {
                eprintln!("comp-backup: {name} is a mirror; skipped (it restores from its source)");
                continue;
            };
            gz.finish()?.flush()?;
            let path = format!("streams/{name}.jsonl.gz");
            let (file, bytes, sha256) =
                tokio::task::block_in_place(|| finalize(&raw, key, &format!("{id}/{path}")))?;
            store.put_file(&format!("{base}/{path}"), &file).await?;
            eprintln!("comp-backup: {name}: {} messages, {bytes} bytes", dumped.messages);
            parts.push(Part {
                kind: "stream".into(),
                name,
                path,
                bytes,
                sha256,
                messages: dumped.messages,
            });
        }
    }
    if let Some(db) = &db {
        for nsdb in &a.surreal.surreal_dbs {
            let (ns, d) = surreal::split(nsdb)?;
            let export = scratch.path().join(format!("{ns}.{d}.surql"));
            db.export(nsdb, &export).await?;
            let raw = scratch.path().join(format!("{ns}.{d}.surql.gz"));
            tokio::task::block_in_place(|| -> Result<()> {
                let mut gz = GzEncoder::new(
                    BufWriter::new(std::fs::File::create(&raw)?),
                    flate2::Compression::default(),
                );
                std::io::copy(&mut BufReader::new(std::fs::File::open(&export)?), &mut gz)?;
                gz.finish()?.flush()?;
                Ok(())
            })?;
            let path = format!("surreal/{ns}.{d}.surql.gz");
            let (file, bytes, sha256) =
                tokio::task::block_in_place(|| finalize(&raw, key, &format!("{id}/{path}")))?;
            store.put_file(&format!("{base}/{path}"), &file).await?;
            eprintln!("comp-backup: surreal {nsdb}: {bytes} bytes");
            parts.push(Part {
                kind: "surreal".into(),
                name: nsdb.clone(),
                path,
                bytes,
                sha256,
                messages: 0,
            });
        }
    }

    let manifest = Manifest {
        format: 1,
        id: id.clone(),
        created: jiff::Timestamp::now().to_string(),
        sealed: key.is_some(),
        key_fingerprint: key.map(Key::fingerprint).unwrap_or_default(),
        parts,
    };
    // The commit point: until this lands, the backup does not exist.
    store
        .put_bytes(&format!("{base}/manifest.json"), serde_json::to_vec_pretty(&manifest)?)
        .await?;
    eprintln!(
        "comp-backup: backup {id} complete — {} parts in {:.1}s",
        manifest.parts.len(),
        started.elapsed().as_secs_f64()
    );
    if a.keep > 0 {
        prune(&store, &a.s3.s3_prefix, a.keep).await?;
    }
    Ok(manifest)
}

/// Every backup id under the prefix, oldest first, with whether it completed.
async fn ids(store: &Store, prefix: &str) -> Result<Vec<(String, bool)>> {
    let root = format!("{}/", prefix.trim_end_matches('/'));
    let mut seen: std::collections::BTreeMap<String, bool> = Default::default();
    for key in store.list(&root).await? {
        let Some((id, rest)) = key[root.len()..].split_once('/') else {
            continue;
        };
        let done = seen.entry(id.to_string()).or_default();
        *done |= rest == "manifest.json";
    }
    Ok(seen.into_iter().collect())
}

/// Keep the newest `keep` complete backups. Delete older complete ones, and
/// incomplete ones older than the newest complete one — a newer incomplete one
/// may be a run still in progress.
async fn prune(store: &Store, prefix: &str, keep: usize) -> Result<()> {
    let all = ids(store, prefix).await?;
    let complete: Vec<&String> = all.iter().filter(|(_, d)| *d).map(|(i, _)| i).collect();
    let Some(newest) = complete.last().map(|s| s.to_string()) else {
        return Ok(());
    };
    let kept: std::collections::BTreeSet<&String> =
        complete.iter().rev().take(keep).copied().collect();
    let root = prefix.trim_end_matches('/');
    for (id, done) in &all {
        let drop = if *done { !kept.contains(id) } else { id < &newest };
        if !drop {
            continue;
        }
        let keys = store.list(&format!("{root}/{id}/")).await?;
        // The manifest first: a half-deleted backup reads as interrupted, never
        // as complete with parts missing.
        let (manifest, rest): (Vec<_>, Vec<_>) =
            keys.into_iter().partition(|k| k.ends_with("/manifest.json"));
        for k in manifest.iter().chain(rest.iter()) {
            store.delete(k).await?;
        }
        eprintln!("comp-backup: pruned {}{id}", if *done { "" } else { "interrupted " });
    }
    Ok(())
}

async fn list(s3: &S3Args) -> Result<()> {
    let store = Store::connect(s3).await?;
    let root = s3.s3_prefix.trim_end_matches('/');
    for (id, done) in ids(&store, root).await? {
        if !done {
            println!("{id}  INTERRUPTED (no manifest)");
            continue;
        }
        let m: Manifest = match store.get_bytes(&format!("{root}/{id}/manifest.json")).await? {
            Some(b) => serde_json::from_slice::<Manifest>(&b)?,
            None => continue,
        };
        let bytes: u64 = m.parts.iter().map(|p| p.bytes).sum();
        println!(
            "{id}  {} parts  {bytes} bytes  {}",
            m.parts.len(),
            if m.sealed { format!("sealed:{}", m.key_fingerprint) } else { "PLAINTEXT".into() }
        );
    }
    Ok(())
}

fn matches_any(globs: &[String], name: &str) -> bool {
    globs.is_empty() || globs.iter().any(|g| streams::glob(g, name))
}

async fn restore(a: RestoreArgs) -> Result<()> {
    let store = Store::connect(&a.s3).await?;
    let root = a.s3.s3_prefix.trim_end_matches('/').to_string();
    let id = if a.id == "latest" {
        ids(&store, &root)
            .await?
            .into_iter()
            .rev()
            .find(|(_, d)| *d)
            .map(|(i, _)| i)
            .context("no complete backup in the bucket")?
    } else {
        a.id.clone()
    };
    let m: Manifest = serde_json::from_slice(
        &store
            .get_bytes(&format!("{root}/{id}/manifest.json"))
            .await?
            .with_context(|| format!("backup {id} has no manifest — it never completed"))?,
    )?;
    let key = match (&a.encrypt_key_file, m.sealed) {
        (Some(p), true) => {
            let k = Key::load(p)?;
            if k.fingerprint() != m.key_fingerprint {
                bail!(
                    "backup {id} was sealed with key {}, not {}",
                    m.key_fingerprint,
                    k.fingerprint()
                );
            }
            Some(k)
        }
        (None, true) => bail!("backup {id} is sealed: pass --encrypt-key-file"),
        (_, false) => None,
    };
    let js = nats(&a.nats).await?;
    let db = Surreal::new(&a.surreal)?;
    let scratch = tempfile::Builder::new().prefix(&format!("comp-backup-restore-{id}-")).tempdir()?;
    eprintln!("comp-backup: restoring {id} ({} parts)", m.parts.len());

    for part in m.parts.iter().filter(|p| matches_any(&a.only, &p.name)) {
        let target_missing = match part.kind.as_str() {
            "stream" => js.is_none(),
            "surreal" => db.is_none(),
            other => bail!("backup {id}: unknown part kind {other}"),
        };
        if target_missing {
            eprintln!("comp-backup: {}: no {} to restore into; skipped", part.name, part.kind);
            continue;
        }
        let file = scratch.path().join(part.path.replace('/', "_"));
        store.get_file(&format!("{root}/{id}/{}", part.path), &file).await?;
        // Size and hash before anything is opened: a part that is not the one
        // the manifest describes is refused whole.
        let (bytes, sha) = tokio::task::block_in_place(|| -> Result<(u64, String)> {
            let mut h = Sha256::new();
            let n = std::io::copy(&mut BufReader::new(std::fs::File::open(&file)?), &mut h)?;
            Ok((n, hex::encode(h.finalize())))
        })?;
        if bytes != part.bytes || sha != part.sha256 {
            bail!("{}: the stored part does not match its manifest (size or SHA-256)", part.path);
        }
        let gz = match &key {
            Some(k) => {
                let opened = file.with_extension("opened");
                tokio::task::block_in_place(|| -> Result<()> {
                    let mut out = BufWriter::new(std::fs::File::create(&opened)?);
                    seal::open(
                        k,
                        &format!("{id}/{}", part.path),
                        BufReader::new(std::fs::File::open(&file)?),
                        &mut out,
                    )?;
                    out.flush()?;
                    Ok(())
                })?;
                opened
            }
            None => file.clone(),
        };
        let decoded = BufReader::new(GzDecoder::new(BufReader::new(std::fs::File::open(&gz)?)));
        match part.kind.as_str() {
            "stream" => {
                let r = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(streams::restore(
                        js.as_ref().unwrap(),
                        decoded,
                        a.replace,
                        a.replicas,
                    ))
                })?;
                if r.messages != part.messages {
                    bail!(
                        "{}: replayed {} messages, the manifest says {}",
                        r.stream,
                        r.messages,
                        part.messages
                    );
                }
                eprintln!("comp-backup: {}: {} messages restored", r.stream, r.messages);
            }
            _ => {
                let mut surql = Vec::new();
                { decoded }.read_to_end(&mut surql)?;
                db.as_ref().unwrap().import(&part.name, surql).await?;
                eprintln!("comp-backup: surreal {}: imported", part.name);
            }
        }
    }
    eprintln!("comp-backup: restore of {id} done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_parse_and_nonsense_does_not() {
        assert_eq!(parse_interval("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_interval("6h").unwrap(), Duration::from_secs(6 * 3600));
        assert_eq!(parse_interval("1d").unwrap(), Duration::from_secs(86400));
        for bad in ["", "6", "h", "0s", "5w", "-1h"] {
            assert!(parse_interval(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn restore_filters_match_like_stream_globs() {
        assert!(matches_any(&[], "KV_x"));
        assert!(matches_any(&["KV_*".into()], "KV_x"));
        assert!(!matches_any(&["KV_*".into()], "OBJ_x"));
        assert!(matches_any(&["vcs/graph".into()], "vcs/graph"));
    }
}
