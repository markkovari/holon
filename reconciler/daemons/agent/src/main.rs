//! Pull-based deploy: by digest (ADR-0006), and only for apps this box already has a unit for, so
//! one commit to the deploy branch cannot start arbitrary services.

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
            "--dry-run" => dry = true,
            "-h" | "--help" => {
                eprintln!(
                    "comp-agent --lock-url <url> [--interval 300] [--once] [--dry-run]\n\n\
                     Keeps the apps this box already runs at the digest the lock names.\n\
                     Installs nothing new: adding an app is a manual `holon node render` \
                     plus a copy to the box, not something this does for you."
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
    /// In the lock, not per box: the scheme is the registry's, and a mismatch fails cryptically.
    scheme: String,
    digests: BTreeMap<String, String>,
}

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
    // curl rather than an HTTP crate: this binary is 11 MB of reconciler already and the box has curl.
    let out = Command::new("curl")
        .args(["-sSL", "--max-time", "30", "-w", "\n%{http_code}", url])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    let body = String::from_utf8_lossy(&out.stdout);
    let (text, code) = body.rsplit_once('\n').unwrap_or(("", "000"));
    if !out.status.success() {
        return Err(format!("fetching {url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    // Say it plainly: curl's bare "exit status: 22" read as a broken agent before the first publish.
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

/// Recorded at install, not hashed: a registry digest covers the manifest, not the bare `.wasm`.
fn installed_digest(app: &str) -> Option<String> {
    std::fs::read_to_string(app_dir(app).join("digest")).ok().map(|s| s.trim().to_string())
}

fn sweep(lock_url: &str, dry: bool) -> Result<usize, String> {
    let desired = parse_lock(&fetch(lock_url)?);
    if desired.registry.is_empty() {
        return Err("the lock names no registry".into());
    }
    // Lazy: `--dry-run` pulls nothing, so it must not fail on a missing puller.
    let mut oci: Option<PathBuf> = None;

    let mut updated = 0;
    for (app, digest) in &desired.digests {
        let dir = app_dir(app);
        // Only apps this box already runs; a missing directory is no reason to install anything.
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
            None => {
                let p = which_oci()?;
                oci = Some(p.clone());
                p
            }
        };
        if let Err(e) = update(&tool, &desired.registry, &desired.scheme, app, digest) {
            // One app failing must not stop the others: they are unrelated services sharing a box.
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
    Err("no comp-oci — build it (`cargo build --release --bin comp-oci` in reconciler/) or install it to /usr/local/bin".into())
}

/// The digest is recorded only AFTER the restart succeeds, or a failed restart would never be retried.
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
    // A component or nothing: anything else would be installed, fail to start, and leave the app down.
    let bytes = std::fs::read(&tmp).map_err(|e| format!("reading the pull: {e}"))?;
    if bytes.len() < 8 || &bytes[..4] != b"\0asm" || bytes[6] != 0x01 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{reference} is not a wasm component"));
    }

    // Rename, not copy: the running host has the old file open and a partial write over it traps.
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

    /// A truncated fetch must not read as "remove every app": an empty lock is a no-op.
    #[test]
    fn an_empty_lock_asks_for_nothing() {
        assert!(parse_lock("# nothing here\n").digests.is_empty());
        assert!(parse_lock("").digests.is_empty());
    }
}
