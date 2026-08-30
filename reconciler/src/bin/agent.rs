//! `comp-agent` — the pull half of continuous deployment.
//!
//! Runs on the box, on a timer. Reads the desired state, compares it to what is
//! installed, and where they differ pulls the artifact BY DIGEST, installs it and
//! restarts the unit. Nothing reaches in from outside: no inbound port, no
//! credential in a CI system that can touch this network, and no webhook.
//!
//! ## What is mutable, and where
//!
//! ADR-0006: nothing in a deploy may reference a tag, because a tag drifts and what
//! ran becomes unknowable. So the artifact is pulled by digest and the digest that
//! was installed is written down.
//!
//! Something has to be mutable or nothing can ever update, and here it is exactly
//! one thing: which commit of `apps.lock` is current. That is a branch in git, whose
//! history IS the deploy history — `git log deploy -- apps.lock` says what was
//! current when, and who merged it. A tag would put the same mutability inside the
//! artifact reference, where nothing can audit it.
//!
//! ## What it will not do
//!
//! It deploys apps this box ALREADY HAS a unit for. An agent that installed new apps
//! because a file said to would mean a commit to one branch of one repository can
//! start arbitrary services on this machine; the blast radius of a compromised CI is
//! then every box running the agent. Adding an app stays a deliberate
//! `just selfhost-deploy`, and this keeps the ones already there current.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut lock_url = String::new();
    let mut once = false;
    let mut interval = 300u64;
    let mut dry = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--lock-url" => {
                i += 1;
                lock_url = args.get(i).cloned().unwrap_or_default();
            }
            "--interval" => {
                i += 1;
                interval = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(300);
            }
            "--once" => once = true,
            // Says what it WOULD do and changes nothing. The first thing to run on a
            // box you have not automated before.
            "--dry-run" => dry = true,
            "-h" | "--help" => {
                eprintln!(
                    "comp-agent --lock-url <url> [--interval 300] [--once] [--dry-run]\n\n\
                     Keeps the apps this box already runs at the digest the lock names.\n\
                     Installs nothing new: adding an app is `just selfhost-deploy`."
                );
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    if lock_url.is_empty() {
        eprintln!("comp-agent: --lock-url is required (see --help)");
        std::process::exit(2);
    }

    loop {
        match sweep(&lock_url, dry) {
            Ok(n) if n > 0 => eprintln!("comp-agent: updated {n} app(s)"),
            Ok(_) => {}
            Err(e) => eprintln!("comp-agent: {e}"),
        }
        if once {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(interval));
    }
}

struct Desired {
    registry: String,
    /// `https` unless the lock says otherwise. Carried in the LOCK rather than
    /// configured on each box: which scheme a registry speaks is a property of that
    /// registry, and the publisher is the one that knows. A box told `https` about a
    /// plaintext registry fails with `received corrupt message of type
    /// InvalidContentType`, which names neither the scheme nor the registry.
    scheme: String,
    digests: BTreeMap<String, String>,
}

/// `# comment`, `registry <ref>`, optional `scheme http`, then `<app> <digest>`.
fn parse_lock(text: &str) -> Desired {
    let mut registry = String::new();
    let mut scheme = "https".to_string();
    let mut digests = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(a), Some(b)) = (parts.next(), parts.next()) else { continue };
        match a {
            "registry" => registry = b.to_string(),
            "scheme" => scheme = b.to_string(),
            _ => {
                digests.insert(a.to_string(), b.to_string());
            }
        }
    }
    Desired { registry, scheme, digests }
}

fn fetch(url: &str) -> Result<String, String> {
    // curl rather than an HTTP crate: this binary is 11 MB of reconciler already and
    // the box has curl, which `selfhost-bootstrap` needed anyway.
    let out = Command::new("curl")
        .args(["-sSL", "--max-time", "30", "-w", "\n%{http_code}", url])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    let body = String::from_utf8_lossy(&out.stdout);
    let (text, code) = body.rsplit_once('\n').unwrap_or(("", "000"));
    if !out.status.success() {
        return Err(format!("fetching {url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    // The status, said plainly. `curl exited exit status: 22` is how this used to
    // report a 404, which reads as a broken agent rather than "nothing has been
    // published yet" — the state every box is in before the first deploy.
    match code {
        "200" => {}
        "404" => {
            return Err(format!(
                "no lock at {url} yet (404) — nothing has been published; \
                 the publish-apps workflow writes it on the first merge to main"
            ))
        }
        other => return Err(format!("fetching {url}: HTTP {other}")),
    }
    Ok(text.to_string())
}

fn app_dir(app: &str) -> PathBuf {
    PathBuf::from("/srv/comp").join(app)
}

/// What is installed, as recorded when it was installed.
///
/// Read from a file rather than by hashing the artifact: the digest a registry
/// gives is over the manifest, not over the bare `.wasm`, so hashing what is on
/// disk would produce a different number and every sweep would redeploy.
fn installed_digest(app: &str) -> Option<String> {
    std::fs::read_to_string(app_dir(app).join("digest")).ok().map(|s| s.trim().to_string())
}

fn sweep(lock_url: &str, dry: bool) -> Result<usize, String> {
    let desired = parse_lock(&fetch(lock_url)?);
    if desired.registry.is_empty() {
        return Err("the lock names no registry".into());
    }
    // Resolved lazily. A dry run changes nothing and pulls nothing, so requiring
    // the puller up front made `--dry-run` — the one command meant to be run FIRST
    // on a box — fail before it could say what it would do.
    let mut oci: Option<PathBuf> = None;

    let mut updated = 0;
    for (app, digest) in &desired.digests {
        let dir = app_dir(app);
        // Only apps this box already runs — see the header. A missing directory is
        // not an error and not a reason to install anything.
        if !dir.is_dir() {
            continue;
        }
        if installed_digest(app).as_deref() == Some(digest.as_str()) {
            continue;
        }
        eprintln!(
            "comp-agent: {app} {} -> {digest}",
            installed_digest(app).unwrap_or_else(|| "(unknown)".into())
        );
        if dry {
            updated += 1;
            continue;
        }
        let tool = match &oci {
            Some(p) => p.clone(),
            None => match which_oci() {
                Ok(p) => {
                    oci = Some(p.clone());
                    p
                }
                Err(e) => return Err(e),
            },
        };
        if let Err(e) = update(&tool, &desired.registry, &desired.scheme, app, digest) {
            // One app failing must not stop the others: they are unrelated services
            // that happen to share a box.
            eprintln!("comp-agent: {app}: {e}");
            continue;
        }
        updated += 1;
    }
    Ok(updated)
}

fn which_oci() -> Result<PathBuf, String> {
    for p in ["/usr/local/bin/comp-oci", "reconciler/target/release/comp-oci"] {
        if Path::new(p).is_file() {
            return Ok(PathBuf::from(p));
        }
    }
    Err("no comp-oci — `just selfhost-bootstrap` installs it".into())
}

/// Pull by digest, install, restart, record.
///
/// The digest is written AFTER the restart succeeds. Writing it first would mean a
/// failed restart left the box claiming a version it is not running, and the next
/// sweep would agree with it and do nothing.
fn update(oci: &Path, registry: &str, scheme: &str, app: &str, digest: &str) -> Result<(), String> {
    let dir = app_dir(app);
    let tmp = dir.join("app.wasm.incoming");
    let reference = format!("{app}@{digest}");

    let out = Command::new(oci)
        .arg("pull")
        .arg(registry)
        .arg(&reference)
        .arg("--out")
        .arg(&tmp)
        .arg("--scheme")
        .arg(scheme)
        .output()
        .map_err(|e| format!("running comp-oci: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "pulling {reference}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // A component or nothing. An artifact that is not one would be installed, fail
    // to start, and leave the app down until somebody read a journal.
    let bytes = std::fs::read(&tmp).map_err(|e| format!("reading the pull: {e}"))?;
    if bytes.len() < 8 || &bytes[..4] != b"\0asm" || bytes[6] != 0x01 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{reference} is not a wasm component"));
    }

    // Rename, not copy: the running host has the old file open, and a partial write
    // over it is an app that starts and traps.
    std::fs::rename(&tmp, dir.join("app.wasm")).map_err(|e| format!("installing: {e}"))?;

    let status = Command::new("systemctl")
        .args(["restart", &format!("comp-{app}")])
        .status()
        .map_err(|e| format!("systemctl: {e}"))?;
    if !status.success() {
        return Err(format!("comp-{app} did not restart"));
    }
    std::fs::write(dir.join("digest"), format!("{digest}\n"))
        .map_err(|e| format!("recording the digest: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_is_a_registry_and_digests() {
        let d = parse_lock(
            "# Generated, do not edit\n\
             # built-from abc123\n\
             registry ghcr.io/owner/holon-apps\n\
             events sha256:aaa\n\
             poll   sha256:bbb\n",
        );
        assert_eq!(d.registry, "ghcr.io/owner/holon-apps");
        assert_eq!(d.digests.get("events").map(String::as_str), Some("sha256:aaa"));
        assert_eq!(d.digests.len(), 2);
        assert_eq!(d.scheme, "https", "https unless the lock says otherwise");

        let plain = parse_lock("registry 10.0.0.1:5055/apps\nscheme http\nevents sha256:c\n");
        assert_eq!(plain.scheme, "http");
        assert_eq!(plain.digests.len(), 1, "`scheme` is not an app");
    }

    /// A truncated fetch must not read as "every app should be removed" — and it
    /// cannot, because the agent only ever acts on entries that ARE present. A lock
    /// with nothing in it is a no-op, which is the safe reading of a bad download.
    #[test]
    fn an_empty_lock_asks_for_nothing() {
        assert!(parse_lock("# nothing here\n").digests.is_empty());
        assert!(parse_lock("").digests.is_empty());
    }
}
