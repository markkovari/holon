//! contrast-audit, live on the lattice: deploy the component, serve the page, and
//! POST a list of colour pairs to /audit — which recomputes every WCAG ratio,
//! reaches Claude by egress with the key from the vault, and returns a real report
//! over the lattice.
//!
//! Ignored by default: it spends money and needs a real key. Run explicitly:
//!   cargo test --release --test contrast_audit_live -- --ignored --nocapture

use std::time::{Duration, Instant};

use comp_reconciler::fleet::{free_port, repo_root, Fleet};
use serde_json::Value;

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let p = dir.join("contrast_audit.wasm");
    assert!(p.exists(), "missing {} — build contrast-audit first", p.display());
    vec![format!("gate={}", p.display())]
}

#[test]
#[ignore]
fn audit_a_palette_and_get_a_report_over_the_lattice() {
    let key_path = dirs_home().join(".comp-secrets/anthropic");
    assert!(key_path.exists(), "need ~/.comp-secrets/anthropic");
    let _ = free_port();

    let fleet = Fleet::start_with_secrets(
        "contrast",
        &[repo_root().join("fixtures/contrast-audit.yaml").to_str().unwrap()],
        &artifacts(),
        &[format!("vault://acme/anthropic=@{}", key_path.display())],
    );

    let http =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(90)).build().unwrap();
    let base = format!("http://127.0.0.1:{}", fleet.ingress_port);

    // 1) the page serves over the lattice
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut served = false;
    while Instant::now() < deadline {
        if let Ok(r) = http.get(&base).header("host", "contrast.acme.test").send() {
            if r.status().is_success() && r.text().unwrap_or_default().contains("Contrast Audit") {
                served = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(served, "the page never served\n{}", fleet.node_log("n1"));
    println!("    page is live over the lattice");

    // 2) one failing pair and one passing one, so the report has both to say.
    //
    // The failing pair carries a LIE: the page claims 21:1 for two greys that are
    // nowhere near it. The component must recompute rather than pass it through,
    // so a report calling that pair fine is a real failure of this test.
    let body = serde_json::json!({
        "pairs": [
            { "fg": "#999999", "bg": "#aaaaaa", "share": 0.4, "ratio": 21.0 },
            { "fg": "#111111", "bg": "#ffffff", "share": 0.3 },
            // Junk, to prove the parse is strict rather than creative.
            { "fg": "rebeccapurple", "bg": "#fff", "share": 0.1 }
        ]
    });
    let r = http
        .post(format!("{base}/audit"))
        .header("host", "contrast.acme.test")
        .json(&body)
        .send()
        .expect("audit");
    let status = r.status();
    let v: Value = r.json().unwrap_or(Value::Null);
    assert!(status.is_success(), "audit failed: {status} {v}\n{}", fleet.node_log("n1"));
    let report = v["report"].as_str().unwrap_or("");
    assert!(
        report.contains("Verdict") || report.contains("Fix first"),
        "no report came back: {v}"
    );
    // The model was told the truth about the grey pair, so its report has to be
    // about a failure. A report that only praises means the client's 21:1 reached
    // the prompt.
    let lower = report.to_lowercase();
    assert!(
        lower.contains("999999") || lower.contains("fail") || lower.contains("4.5"),
        "the failing pair is missing from the report — was the client's ratio trusted?\n{report}"
    );
    println!("    report over the lattice:\n{report}");
}

/// A request with nothing auditable in it is an error, not an empty report.
///
/// Cheap and unignored: it never reaches the model, so it costs nothing and it
/// guards the path that a real page hits the moment it samples a flat image.
#[test]
fn a_request_with_no_usable_pairs_is_refused() {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    if !dir.join("contrast_audit.wasm").exists() {
        eprintln!("contrast-audit not built; skipping");
        return;
    }
    // A PLACEHOLDER key, never revealed.
    //
    // Not "no secret at all", which is what this test asked for first: the
    // manifest declares the grant, so the host refuses to START the component
    // without one — `cannot start: secret "anthropic-api-key" -> no such secret`,
    // repeated until the deploy times out. The grant is a precondition of running,
    // not something checked at the moment it is read. So the fleet gets a
    // throwaway value, and the request is still refused long before anything would
    // reveal it — which is what keeps this test free.
    let fake = std::env::temp_dir().join("comp-contrast-audit-placeholder-key");
    std::fs::write(&fake, "not-a-real-key").expect("write placeholder key");
    let fleet = Fleet::start_with_secrets(
        "contrast",
        &[repo_root().join("fixtures/contrast-audit.yaml").to_str().unwrap()],
        &artifacts(),
        &[format!("vault://acme/anthropic=@{}", fake.display())],
    );
    let http =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
    let base = format!("http://127.0.0.1:{}", fleet.ingress_port);
    // Wait for the PAGE, not merely for a reply. The ingress answers 503 while the
    // app is still coming up, and `send().is_ok()` is true for that — so a loop
    // that breaks on any response goes on to assert against the ingress's error
    // instead of the component's, and fails somewhere that says nothing.
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut served = false;
    while Instant::now() < deadline {
        if let Ok(r) = http.get(&base).header("host", "contrast.acme.test").send() {
            if r.status().is_success() && r.text().unwrap_or_default().contains("Contrast Audit") {
                served = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(served, "the page never served\n{}", fleet.node_log("n1"));
    // Two identical colours and one unparseable pair: nothing survives the audit.
    let body = serde_json::json!({
        "pairs": [
            { "fg": "#404040", "bg": "#404040", "share": 0.9 },
            { "fg": "#gggggg", "bg": "#ffffff", "share": 0.1 }
        ]
    });
    let r = http
        .post(format!("{base}/audit"))
        .header("host", "contrast.acme.test")
        .json(&body)
        .send()
        .expect("audit");
    assert_eq!(r.status().as_u16(), 500, "an unauditable request must not read as a report");
    let v: Value = r.json().unwrap_or(Value::Null);
    let err = v["error"].as_str().unwrap_or("");
    assert!(
        err.contains("different colours"),
        "the refusal must say what was wrong with the request, got {v}"
    );
}

fn dirs_home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap())
}
