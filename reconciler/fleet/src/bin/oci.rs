//! Push a component to an OCI registry and pull one back; digests, not tags (ADR-0006, ADR-0024).

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use comp_oci::{self as oci, Creds};

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

/// Not recursive: `components/target` holds intermediates nobody meant to publish.
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

/// Best-effort: the bytes are the artifact, so a module with no surface yields empty lists.
fn surface(wasm: &[u8]) -> (Vec<String>, Vec<String>) {
    match comp_fleet::plug::surface(wasm) {
        Ok(s) => (
            s.exports.into_iter().collect(),
            // Both kinds: a puller wants everything the component will reach for.
            s.imports.into_iter().chain(s.host_imports).collect(),
        ),
        Err(_) => (Vec::new(), Vec::new()),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let http = reqwest::Client::builder()
        // A 17 MB Python build over a home connection outlasts the default.
        .timeout(std::time::Duration::from_secs(300))
        .build()?;
    let creds = Creds::from_env();

    match args.cmd {
        Cmd::Push { registry, paths, lock } => {
            // Only on push: anonymous is the NORMAL case for pull, so warning there is noise.
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
                    // The repo builds `portfolio_value.wasm`; a registry path reads better with the catalogue's name.
                    .replace('_', "-");
                let wasm =
                    std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
                let (exports, imports) = surface(&wasm);
                let repo = repo_for(&prefix, &name);
                let digest = oci::push_artifact(
                    &http,
                    &base,
                    &repo,
                    &wasm,
                    &exports,
                    &imports,
                    creds.as_ref(),
                )
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
            // `name@sha256:…` first: a digest cannot drift, so it wins over the `:tag` split (and contains a colon).
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
            std::fs::write(&out, &wasm).with_context(|| format!("writing {}", out.display()))?;
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
