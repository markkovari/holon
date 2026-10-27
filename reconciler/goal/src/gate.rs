use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use holon_fleet::fleet::{bin_path, free_port};

pub fn egress_authority(base_url: &str) -> String {
    let rest = base_url.split_once("://").map(|(_, r)| r).unwrap_or(base_url);
    rest.split('/').next().unwrap_or(rest).to_string()
}

/// Minted per run: a token for a runner this process starts and kills need not outlive it.
pub fn mint_token() -> Result<String> {
    let mut b = [0u8; 32];
    std::io::Read::read_exact(
        &mut std::fs::File::open("/dev/urandom").context("opening /dev/urandom")?,
        &mut b,
    )
    .context("reading /dev/urandom")?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Resolved, not string-matched; an unresolvable name is not loopback, so it demands a token.
pub fn authority_is_loopback(authority: &str) -> bool {
    use std::net::ToSocketAddrs;
    let with_port =
        if authority.contains(':') { authority.to_string() } else { format!("{authority}:80") };
    match with_port.to_socket_addrs() {
        Ok(it) => {
            let a: Vec<_> = it.collect();
            !a.is_empty() && a.iter().all(|x| x.ip().is_loopback())
        }
        Err(_) => false,
    }
}

/// Everything downstream reads the same answers from here, so a remote gate cannot be half-wired.
#[derive(Debug)]
pub enum Gate {
    /// Started here, killed with the run.
    Local(Checks),
    Remote {
        url: String,
        token_file: PathBuf,
    },
}

impl Gate {
    pub fn open(
        checks_url: Option<&str>,
        checks_token_file: Option<&Path>,
        check_timeout: u64,
        allow: &[&str],
        check_env: &[String],
    ) -> Result<Self> {
        let Some(url) = checks_url.map(str::to_string) else {
            return Ok(Gate::Local(Checks::start(allow, check_env, check_timeout)?));
        };
        if !url.starts_with("http://") && !url.starts_with("https://") {
            bail!("--checks-url must start with http:// or https://, got {url:?}");
        }
        let authority = egress_authority(&url);
        let token_file = match (&checks_token_file, authority_is_loopback(&authority)) {
            (Some(p), _) => {
                if !p.is_file() {
                    bail!("--checks-token-file {} does not exist", p.display());
                }
                p.to_path_buf()
            }
            // Mirrors `comp-checks`: otherwise every candidate 401s, reading as a broken gate.
            (None, false) => bail!(
                "--checks-url {url} is not on this machine, so it needs --checks-token-file.\n\
                 \n\
                 The runner at the other end refuses to listen off the loopback without a token \
                 for the same reason: --allow bounds the COMMAND, not the tree it runs over.\n\
                 \n\
                 \x20 head -c 32 /dev/urandom | base64 > ~/.comp-secrets/checks   # on both boxes"
            ),
            // Its own guard already allows this; refusing here would be a second opinion.
            (None, true) => {
                let dir = std::env::temp_dir().join(format!("comp-goalrun-{}", std::process::id()));
                std::fs::create_dir_all(&dir)?;
                let p = dir.join("checks-token");
                std::fs::write(&p, "")?;
                p
            }
        };
        eprintln!("goalrun: gate is {url} (not started here)");
        Ok(Gate::Remote { url, token_file })
    }

    pub fn url(&self) -> String {
        match self {
            Gate::Local(c) => format!("http://127.0.0.1:{}/check", c.port),
            Gate::Remote { url, .. } => url.clone(),
        }
    }

    /// A manifest decision, so the real host, not a stand-in (ADR-0008).
    pub fn authority(&self) -> String {
        egress_authority(&self.url())
    }

    pub fn token_file(&self) -> &Path {
        match self {
            Gate::Local(c) => &c.token_file,
            Gate::Remote { token_file, .. } => token_file,
        }
    }

    /// For the one call made from here: the composition gate.
    pub fn token(&self) -> Option<String> {
        std::fs::read_to_string(self.token_file())
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    }
}

#[derive(Debug)]
pub struct Checks {
    child: Child,
    pub port: u16,
    token_file: PathBuf,
    _dir: tempfile::TempDir,
}
impl Drop for Checks {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Checks {
    pub fn start(allow: &[&str], check_env: &[String], timeout: u64) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let port = free_port();
        // Authenticated even on loopback, so the remote path is the one used every day.
        let token_file = dir.path().join("token");
        std::fs::write(&token_file, mint_token()?)?;
        // A subdirectory, so throwaway trees never share a parent with the token file.
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work)?;
        let mut cmd = Command::new(bin_path("comp-checks"));
        cmd.args(["--addr", &format!("127.0.0.1:{port}")])
            .arg("--work-dir")
            .arg(&work)
            .arg("--token-file")
            .arg(&token_file)
            .args(["--timeout", &timeout.to_string()]);
        for a in allow {
            cmd.args(["--allow", a]);
        }
        for e in check_env {
            cmd.args(["--check-env", e]);
        }
        let child = cmd.stdout(Stdio::null()).stderr(Stdio::inherit()).spawn()?;
        let me = Self { child, port, token_file, _dir: dir };
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Ok(me);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        bail!("comp-checks never listened on {port}");
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A substring test for `127.` accepts `10.0.127.4` and rejects `::1`.
    #[test]
    fn what_counts_as_only_this_machine() {
        for local in ["127.0.0.1:8099", "localhost:8099", "[::1]:8099", "127.9.9.9:1"] {
            assert!(authority_is_loopback(local), "{local} is loopback");
        }
        for remote in ["0.0.0.0:8099", "10.0.127.4:8099", "example.invalid:8099"] {
            assert!(!authority_is_loopback(remote), "{remote} is not");
        }
    }

    #[test]
    fn a_minted_token_is_random_and_hex() {
        let a = mint_token().expect("/dev/urandom");
        let b = mint_token().expect("/dev/urandom");
        assert_eq!(a.len(), 64, "32 bytes as hex: {a}");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "{a}");
        assert_ne!(a, b, "two runs minted the same token");
    }

    #[test]
    fn a_remote_gate_is_refused_when_it_cannot_be_reached_safely() {
        let e = Gate::open(Some("malna:8199/check"), None, 30, &["true"], &[])
            .expect_err("a url with no scheme");
        assert!(format!("{e}").contains("http://"), "{e}");

        let e = Gate::open(Some("http://example.invalid:8199/check"), None, 30, &["true"], &[])
            .expect_err("off the loopback with no token");
        let said = format!("{e}");
        assert!(said.contains("--checks-token-file"), "it must name the flag: {said}");
        assert!(
            said.contains("--allow bounds the COMMAND"),
            "and why, or 'needs a token' reads as bureaucracy: {said}"
        );

        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        let e = Gate::open(
            Some("http://example.invalid:8199/check"),
            Some(&missing),
            30,
            &["true"],
            &[],
        )
        .expect_err("a token file that is not there");
        assert!(format!("{e}").contains("does not exist"), "{e}");

        // A guard that broke every local gate would get deleted.
        let g = Gate::open(Some("http://127.0.0.1:8199/check"), None, 30, &["true"], &[])
            .expect("loopback without a token is a supported setup");
        assert_eq!(g.url(), "http://127.0.0.1:8199/check");
        assert_eq!(g.authority(), "127.0.0.1:8199");
        assert!(g.token().is_none(), "no token was granted, so none is presented");
    }
}
