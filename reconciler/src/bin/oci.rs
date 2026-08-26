//! `comp-oci` — put a built component in a registry, and get one back out.
//!
//! ```
//! comp-oci push ghcr.io/markkovari/holon components/target/wasm32-wasip2/release/*.wasm
//! comp-oci pull ghcr.io/markkovari/holon portfolio-value-c -o pv.wasm
//! comp-oci pull ghcr.io/markkovari/holon portfolio-value-c@sha256:… -o pv.wasm
//! ```
//!
//! ## Why this exists
//!
//! Push has been in `reconciler/src/oci.rs` since ADR-0017 with nothing to pull it
//! back, so the only ways to obtain a component's bytes were to build it or to run
//! `just fetch-components`. Building now means five toolchains, one of them a 200 MB
//! wasi-sdk and one a gigabyte of .NET (`docs/POLYGLOT.md`). `fetch-components`
//! reads GitHub Actions artifacts, which expire after thirty days, need a green run
//! for that exact commit, and arrive as all 205 components or none.
//!
//! None of those is a way to get ONE component you did not build. This is.
//!
//! ## What it is not
//!
//! Not on the runtime path. ADR-0024 moved distribution to a JetStream object store
//! keyed by sha256 and that is unchanged: a node still fetches by digest and never
//! talks to a registry. This is for people and for CI.
//!
//! ## Digests, not tags
//!
//! Every push is tagged with the first twelve hex of the component's own sha256, so
//! a tag can never change meaning under someone (ADR-0006), and it also writes a
//! `latest` tag purely so a human can type a name. What it PRINTS is the digest, and
//! `--lock` writes the digest for every component pushed. Pull verifies the bytes
//! against the digest the manifest named before it writes a file, because a registry
//! is a cache and the digest is the trust boundary (ADR-0024).

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use comp_reconciler::oci::{self, Creds};

#[derive(Parser)]
#[command(name = "comp-oci", about = "Push and pull components by digest")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,

    /// `http` for a local registry with no TLS. Anything real is `https`.
    #[arg(long, default_value = "https", global = true)]
    scheme: String,
}

#[derive(Subcommand)]
enum Cmd {
    /// Push components. `OCI_USER` / `OCI_PASSWORD` if the registry asks.
    Push {
        /// `ghcr.io/owner/repo` — the prefix each component's name is appended to.
        registry: String,
        /// The `.wasm` files. A directory pushes every `.wasm` directly inside it.
        paths: Vec<PathBuf>,
        /// Write `<name> <digest>` per line, so a caller can pin what it just pushed.
        #[arg(long)]
        lock: Option<PathBuf>,
    },
    /// Pull one component. `name`, `name:tag`, or `name@sha256:…`.
    Pull {
        registry: String,
        reference: String,
        #[arg(short, long)]
        out: PathBuf,
    },
}

/// Split `ghcr.io/owner/repo` into the host to talk to and the repo prefix.
fn split_registry(registry: &str) -> (String, String) {
    match registry.split_once('/') {
        Some((host, prefix)) => (host.to_string(), prefix.to_string()),
        // No prefix: every component sits at the root of the registry.
        None => (registry.to_string(), String::new()),
    }
}

fn repo_for(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

/// Every `.wasm` a path names: the file itself, or the ones directly inside a
/// directory. Not recursive — `components/target` has composed artifacts and build
/// intermediates under it, and sweeping those up would publish things nobody meant
/// to publish.
fn wasm_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut here: Vec<PathBuf> = std::fs::read_dir(p)
                .with_context(|| format!("reading {}", p.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "wasm"))
                .collect();
            here.sort();
            out.extend(here);
        } else if p.is_file() {
            out.push(p.clone());
        } else {
            bail!("no such path: {}", p.display());
        }
    }
    if out.is_empty() {
        bail!("nothing to push");
    }
    Ok(out)
}

/// The component's surface, for the OCI config blob.
///
/// `plug::surface` is the reader this repository already uses to decide what can be
/// plugged into what, so the strings a puller sees are the same strings the composer
/// reasons about rather than a second opinion from another parser.
///
/// Best-effort on purpose: the BYTES are the artifact. A core module or an adapter
/// sitting in the same directory has no surface and is still worth publishing, so a
/// failure here is an empty list rather than a refusal.
fn surface(wasm: &[u8]) -> (Vec<String>, Vec<String>) {
    match comp_reconciler::plug::surface(wasm) {
        Ok(s) => (
            s.exports.into_iter().collect(),
            // Both kinds, because a puller wants to know everything this thing will
            // reach for — and `docs/POLYGLOT.md` is exactly about host imports a
            // component acquired without asking.
            s.imports.into_iter().chain(s.host_imports).collect(),
        ),
        Err(_) => (Vec::new(), Vec::new()),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let http = reqwest::Client::builder()
        // A component is megabytes and a registry can be slow; the default is not
        // generous enough for a 17 MB Python build over a home connection.
        .timeout(std::time::Duration::from_secs(300))
        .build()?;
    let creds = Creds::from_env();

    match args.cmd {
        Cmd::Push { registry, paths, lock } => {
            // Only on push. Anonymous is the NORMAL case for pull — it is how you
            // fetch anything public — so warning about it there is noise.
            if creds.is_none() {
                eprintln!("no OCI_USER/OCI_PASSWORD set; a push will almost certainly be refused");
            }
            let (host, prefix) = split_registry(&registry);
            let base = format!("{}://{host}", args.scheme);
            let files = wasm_files(&paths)?;
            let mut lines = Vec::new();
            for path in &files {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .with_context(|| format!("unnameable path {}", path.display()))?
                    // The repo builds `portfolio_value.wasm`; a registry path reads
                    // better with the name the catalogue uses.
                    .replace('_', "-");
                let wasm = std::fs::read(path)
                    .with_context(|| format!("reading {}", path.display()))?;
                let (exports, imports) = surface(&wasm);
                let repo = repo_for(&prefix, &name);
                let digest =
                    oci::push_artifact(&http, &base, &repo, &wasm, &exports, &imports, creds.as_ref())
                        .await
                        .with_context(|| format!("pushing {name}"))?;
                println!("{name} {digest} ({} KB)", wasm.len() / 1024);
                lines.push(format!("{name} {digest}"));
            }
            if let Some(lock) = lock {
                std::fs::write(&lock, lines.join("\n") + "\n")
                    .with_context(|| format!("writing {}", lock.display()))?;
                eprintln!("pinned {} component(s) in {}", lines.len(), lock.display());
            }
        }

        Cmd::Pull { registry, reference, out } => {
            let (host, prefix) = split_registry(&registry);
            let base = format!("{}://{host}", args.scheme);
            // `name@sha256:…` first: a digest is the reference that cannot drift, so
            // it wins over the `:tag` split, and a digest contains a colon itself.
            let (name, reference) = match reference.split_once('@') {
                Some((n, d)) => (n.to_string(), d.to_string()),
                None => match reference.split_once(':') {
                    Some((n, t)) => (n.to_string(), t.to_string()),
                    None => (reference.clone(), "latest".to_string()),
                },
            };
            let repo = repo_for(&prefix, &name);
            let wasm = oci::pull_artifact(&http, &base, &repo, &reference, creds.as_ref())
                .await
                .with_context(|| format!("pulling {repo}:{reference}"))?;
            if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).ok();
            }
            std::fs::write(&out, &wasm)
                .with_context(|| format!("writing {}", out.display()))?;
            println!(
                "{} <- {repo}:{reference} ({} KB, {})",
                out.display(),
                wasm.len() / 1024,
                oci::digest_of(&wasm)
            );
        }
    }
    Ok(())
}
