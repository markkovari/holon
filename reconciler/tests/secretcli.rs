//! `comp secret set` against a real control plane, to check it reaches a real vault rather than a
//! plausible-looking endpoint.
//!
//! The VALUE never appears as an argument: an argument is in `~/.bash_history` and in `ps` output for
//! every other user, and neither can be withdrawn. So the test pipes it on stdin, which also checks the
//! tool accepts it that way.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use comp_fleet::fleet::{repo_root, Fleet};

/// The CLI under test: `target/release/holon`, unless `COMP_COMP_BIN` names another.
fn comp_bin() -> std::path::PathBuf {
    std::env::var("COMP_COMP_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| repo_root().join("target/release/holon"))
}

/// Stands in for an API key. Long enough that finding it in an unexpected place is unambiguous.
const KEY: &str = "sk-proj-the-value-that-must-not-appear-in-argv";

struct Cli {
    bin: std::path::PathBuf,
    creds: std::path::PathBuf,
    url: String,
}

impl Cli {
    /// Run `comp ...`, feeding `stdin` if given. Returns (ok, stdout+stderr).
    fn run(&self, args: &[&str], stdin: Option<&str>) -> (bool, String) {
        let mut c = Command::new(&self.bin);
        c.args(args)
            // A per-test credentials file, so this never touches the developer's own session; it is
            // where the platform URL lives after login.
            .env("COMP_CREDENTIALS", &self.creds)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().expect("running the comp binary");
        if let Some(s) = stdin {
            child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }
}

#[test]
fn a_key_goes_into_the_vault_from_stdin_and_never_from_argv() {
    let fleet = Fleet::start_with_platform("secretcli", 1);
    let dir = tempfile::tempdir().unwrap();
    let cli = Cli {
        bin: comp_bin(),
        creds: dir.path().join("credentials.json"),
        url: fleet.platform_url(),
    };
    assert!(cli.bin.exists(), "missing {} — cargo build --release in cli/", cli.bin.display());

    // The control plane is a component and comes up when its host does.
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    let mut up = false;
    while std::time::Instant::now() < deadline && !up {
        up = reqwest::blocking::get(&cli.url).is_ok();
        if !up {
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    assert!(up, "the control plane never answered");

    let (ok, out) = cli.run(
        &[
            "login",
            "--url",
            &cli.url,
            "--email",
            "ada@cli.test",
            "--password",
            "password123",
            "--register",
        ],
        None,
    );
    assert!(ok, "register/login failed: {out}");

    let (ok, out) = cli.run(&["secret", "set", "openai"], Some(KEY));
    assert!(ok, "storing the secret failed: {out}");
    assert!(
        out.contains("vault://") && out.contains("/openai"),
        "the tool should print the reference a manifest carries: {out}"
    );
    // The value is not echoed: the caller has it already and printing it adds a scrollback copy.
    assert!(!out.contains(KEY), "the tool printed the secret back: {out}");

    let (ok, listed) = cli.run(&["secret", "ls"], None);
    assert!(ok, "listing failed: {listed}");
    assert!(listed.contains("openai"), "the stored secret is not listed: {listed}");
    assert!(
        !listed.contains(KEY),
        "listing must never return values — there is no endpoint that does: {listed}"
    );

    // A trailing newline is what every editor and `echo` adds, and a bearer token carrying one fails
    // auth with a message that says nothing about a newline.
    let (ok, _) = cli.run(&["secret", "set", "trimmed"], Some("value\n\n"));
    assert!(ok, "a value with a trailing newline should store");

    // An empty value is a mistake, not a secret: storing it would let a component start with a blank
    // key and fail later, less obviously.
    let (ok, out) = cli.run(&["secret", "set", "empty"], Some("\n"));
    assert!(!ok, "an empty value should be refused, not stored: {out}");

    let (ok, out) = cli.run(&["secret", "rm", "trimmed"], None);
    assert!(ok, "delete failed: {out}");
    let (_, listed) = cli.run(&["secret", "ls"], None);
    assert!(!listed.contains("trimmed"), "the deleted secret is still listed: {listed}");
    assert!(listed.contains("openai"), "deleting one took the other with it: {listed}");

    println!("    stored a key from stdin, listed it by name, and never saw its value");
}
