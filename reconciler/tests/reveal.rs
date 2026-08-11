//! `comp:secrets/reader`, end to end: a manifest grants a key, a guest reveals it.
//!
//! ADR-0051 designed this and the platform half was built and tested — tokens, scope,
//! 401/403 — but the READER was never wired into the host. `secrets::fetch` had no
//! callers, `comp:secrets/reader` was on neither the linker nor `HOST_IFACES`, and no
//! component in the repo imported it. Every claim in that ADR about what a guest sees
//! was therefore untested. These are the three assertions that make it true or not:
//!
//!   1. a granted key comes back as a handle carrying the manifest's name;
//!   2. a key that was not granted comes back as `none` — no error, no other tenant's
//!      secret, and no string the guest can send that reaches one;
//!   3. `reveal` returns the value the vault holds.
//!
//! And the fourth, which is the gap ADR-0051 admitted and did not close: a reference
//! that does not resolve stops the instance at START, not on the first request.

use std::time::Duration;

use comp_reconciler::fleet::Fleet;

fn artifacts() -> Vec<String> {
    let wasm = comp_reconciler::fleet::repo_root()
        .join("components/target/wasm32-wasip2/release/secret_probe.wasm");
    assert!(wasm.exists(), "missing {} — run `just build`", wasm.display());
    vec![format!("gate={}", wasm.display())]
}

/// Ask the probe something, through the ingress. `None` until it serves.
fn probe(fleet: &Fleet, host: &str, path: &str) -> Option<serde_json::Value> {
    let client =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(8)).build().unwrap();
    let r = client
        .get(format!("http://127.0.0.1:{}{path}", fleet.ingress_port))
        .header("host", host)
        .send()
        .ok()?;
    serde_json::from_str(&r.text().ok()?).ok()
}

fn poll(fleet: &Fleet, host: &str, path: &str, within: Duration) -> serde_json::Value {
    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        if let Some(v) = probe(fleet, host, path) {
            return v;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!(
        "{host}{path} never answered\n--- node ---\n{}\n--- reconciler ---\n{}",
        fleet.node_log("n1"),
        fleet.reconciler_log()
    );
}

#[test]
fn a_granted_key_reveals_its_value_and_an_ungranted_one_is_absent() {
    let fleet = Fleet::start_with_secrets(
        "reveal",
        &["fixtures/secret-reveal.yaml"],
        &artifacts(),
        &["vault://acme/stripe=sk_live_e2e".to_string()],
    );

    // The handle first. `name` is read back OFF the handle, so this also says the
    // host put the manifest's key on it rather than echoing the guest's string.
    let has = poll(&fleet, "probe.acme.test", "/has?k=stripe", Duration::from_secs(90));
    assert_eq!(has["granted"], serde_json::json!(true), "the granted key was not granted: {has}");
    assert_eq!(has["name"], serde_json::json!("stripe"), "the handle lost its key: {has}");

    // The boundary. `billing` is a perfectly good key — for somebody else. A guest
    // cannot name a REFERENCE at all, so there is no string that widens this.
    let no = poll(&fleet, "probe.acme.test", "/has?k=billing", Duration::from_secs(30));
    assert_eq!(no["granted"], serde_json::json!(false), "a key nobody granted resolved: {no}");
    assert!(no["value"].is_null(), "an ungranted key carried a value: {no}");

    // And the value itself, which is the half that was never wired.
    let v = poll(&fleet, "probe.acme.test", "/reveal?k=stripe", Duration::from_secs(30));
    assert_eq!(v["value"], serde_json::json!("sk_live_e2e"), "reveal did not return it: {v}");

    // The audit line, which is the reason `reveal` is a separate call from `get`.
    // Key and identity, never a value — a log that carried the plaintext would undo
    // the whole design.
    let log = fleet.node_log("n1");
    assert!(log.contains("secret.reveal"), "the reveal was not audited:\n{log}");
    assert!(log.contains("\"key\":\"stripe\""), "the audit line does not name the key:\n{log}");
    assert!(
        !log.contains("sk_live_e2e"),
        "THE HOST LOGGED THE SECRET. Everything else here is decoration if this fails."
    );
    println!("    revealed, audited, and the value is not in the log");
}

#[test]
fn a_reference_that_does_not_resolve_stops_the_instance_at_start() {
    // Same fixture shape, empty vault. The reference is well-formed and granted; it
    // just does not exist — a secret that was deleted, or a manifest with a typo.
    let fleet =
        Fleet::start_with_secrets("nosecret", &["fixtures/secret-missing.yaml"], &artifacts(), &[]);

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut log = String::new();
    while std::time::Instant::now() < deadline {
        log = fleet.node_log("n1");
        if log.contains("cannot start") {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(log.contains("cannot start"), "it started with a broken reference:\n{log}");
    // The refusal has to name the key AND the reference, or an operator with five
    // secrets on one component is left guessing which one is broken.
    for expected in ["stripe", "vault://acme/gone"] {
        assert!(log.contains(expected), "the refusal does not name {expected:?}:\n{log}");
    }
    assert!(
        probe(&fleet, "gone.acme.test", "/has?k=stripe").is_none(),
        "a component whose secret does not resolve served a request anyway"
    );
    println!("    refused at start, and the reason names the reference");
}
