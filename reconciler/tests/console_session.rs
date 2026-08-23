//! The console's session exchange, against a stand-in platform.
//!
//! `console-domain` is a client of `platform-domain`, not a second control
//! plane — so the interesting claims are all about what it does with somebody
//! else's credential:
//!
//!   1. A login is forwarded, and the token comes back as an **`HttpOnly`
//!      cookie** and **not in the response body**. The console renders
//!      model-written prose; a token any script can read is the wrong thing to
//!      have on that page, and this is the assertion that keeps it true.
//!   2. A proxied read carries that token to the platform as
//!      `Authorization: Bearer` — so the platform sees exactly what the CLI
//!      sends, and one place decides who anyone is.
//!   3. Authoring a goal with no session is refused **before** the forge is
//!      called, because a pull request is not something an anonymous caller
//!      gets to open.
//!
//! The platform here is a stand-in over `TcpListener`, for the same reason
//! `inference.rs` uses one: the assertion is about the header the console SENT,
//! and a real platform cannot be asked what it received.
//!
//! Skipped, loudly, when the composed artifact is missing — `just compose-console`
//! builds it.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use comp_reconciler::fleet::{free_port, repo_root};

/// The token the stand-in platform issues. It appears in no config and no
/// manifest, so a body that contains it can only have got it from the exchange.
const TOKEN: &str = "tok-only-ever-in-the-cookie";

/// What the stand-in saw on one request.
struct Seen {
    path: String,
    authorization: String,
}

/// A platform that answers `/api/login` and `/api/me`, and reports what it was sent.
fn stand_in_platform(port: u16) -> mpsc::Receiver<Seen> {
    let (tx, rx) = mpsc::channel();
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind the stand-in platform");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut start = String::new();
            let _ = reader.read_line(&mut start);
            let path = start.split_whitespace().nth(1).unwrap_or("/").to_string();

            let mut authorization = String::new();
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    // Lowercase the NAME to match, keep the VALUE as sent —
                    // lowercasing the value would also lowercase the token and
                    // make a mangled one compare equal to the real thing.
                    match name.trim().to_ascii_lowercase().as_str() {
                        "authorization" => authorization = value.trim().to_string(),
                        "content-length" => length = value.trim().parse().unwrap_or(0),
                        _ => {}
                    }
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }
            let mut body = vec![0u8; length];
            let _ = std::io::Read::read_exact(&mut reader, &mut body);

            let answer = if path.starts_with("/api/login") {
                serde_json::json!({ "token": TOKEN, "subject": "someone@example.com" }).to_string()
            } else {
                serde_json::json!({ "subject": "someone@example.com" }).to_string()
            };
            let _ = tx.send(Seen { path, authorization });
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                )
                .as_bytes(),
            );
            let _ = stream.flush();
        }
    });
    rx
}

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn composed() -> Option<std::path::PathBuf> {
    let out = Command::new(repo_root().join("reconciler/target/release/comp-plug"))
        .arg("console-domain")
        .current_dir(repo_root())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let p = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    p.exists().then_some(p)
}

#[test]
fn the_token_lands_in_a_cookie_and_never_in_the_body() {
    let Some(artifact) = composed() else {
        eprintln!("SKIPPED: no composed console — run `just compose-console`");
        return;
    };
    let host_bin = repo_root().join("host/target/release/comp-host");
    if !host_bin.exists() {
        eprintln!("SKIPPED: no comp-host binary — run `just build`");
        return;
    }

    let platform_port = free_port();
    let seen = stand_in_platform(platform_port);
    let console_port = free_port();

    let child = Command::new(&host_bin)
        .current_dir(repo_root())
        .args(["--app", "console", "--config", "default-tenant=console"])
        .args(["--config", &format!("platform-url=http://127.0.0.1:{platform_port}")])
        // The stand-in is on loopback, which the host denies by default (ADR-0008).
        .args(["--egress", &format!("127.0.0.1:{platform_port}")])
        .arg("--allow-private-egress")
        .args(["--component", artifact.to_str().unwrap()])
        .args(["--addr", &format!("127.0.0.1:{console_port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn comp-host");
    let _host = Host(child);

    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        // No automatic cookie jar (the feature is not enabled): every request
        // below sets the header itself, which is what the assertions are about.
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{console_port}");

    // Wait for the SPA, which needs no egress — so a failure here is the host or
    // the component, never the stand-in.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if http.get(&base).send().map(|r| r.status().is_success()).unwrap_or(false) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the console never served the SPA");
        std::thread::sleep(Duration::from_millis(250));
    }

    // --- 1. the login exchange ------------------------------------------------
    let r = http
        .post(format!("{base}/api/session"))
        .header("content-type", "application/json")
        .body(r#"{"email":"someone@example.com","password":"hunter2"}"#)
        .send()
        .expect("login");
    assert!(r.status().is_success(), "login failed: {}", r.status());

    let cookie =
        r.headers().get("set-cookie").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let body = r.text().unwrap_or_default();

    assert!(cookie.contains(TOKEN), "the token is not in the cookie: {cookie:?}");
    assert!(cookie.contains("HttpOnly"), "the session cookie is readable by script: {cookie:?}");
    assert!(cookie.contains("SameSite=Strict"), "no SameSite on the session cookie: {cookie:?}");
    assert!(
        !body.contains(TOKEN),
        "THE TOKEN IS IN THE RESPONSE BODY — the whole point of the cookie is that \
         the page cannot read it, and this page renders model-written prose: {body}"
    );

    // --- 2. the token reaches the platform as a bearer ------------------------
    let login = seen.recv_timeout(Duration::from_secs(5)).expect("the platform saw no login");
    assert_eq!(login.path, "/api/login");
    assert!(login.authorization.is_empty(), "a login must not carry a bearer token");

    let session_cookie = cookie.split(';').next().unwrap_or_default().to_string();
    let r = http
        .get(format!("{base}/api/session"))
        .header("cookie", &session_cookie)
        .send()
        .expect("whoami");
    assert!(r.status().is_success(), "whoami failed: {}", r.status());

    let me = seen.recv_timeout(Duration::from_secs(5)).expect("the platform saw no /api/me");
    assert_eq!(me.path, "/api/me");
    assert_eq!(
        me.authorization,
        format!("Bearer {TOKEN}"),
        "the cookie did not become a bearer token on the way to the platform — the \
         platform would see an anonymous request and one place would stop deciding \
         who anyone is"
    );

    // --- 3. authoring is refused before the forge is touched ------------------
    let r = http
        .post(format!("{base}/api/goals"))
        .header("content-type", "application/json")
        .body(r#"{"project":"p","title":"t","spec":"s"}"#)
        .send()
        .expect("author without a session");
    assert_eq!(
        r.status().as_u16(),
        401,
        "an anonymous caller was allowed past the session check — the next thing \
         down that path opens a pull request"
    );

    println!(
        "\n  token in an HttpOnly cookie, bearer to the platform, authoring refused anonymously\n"
    );
}
