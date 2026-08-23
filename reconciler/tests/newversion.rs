//! Recompile, upload, and watch the fleet stop running the old code.
//!
//! Everything else here tests DEPLOYING an artifact. This tests REPLACING one,
//! which is the harder half and the one a graph loop needs: an agent that
//! produces a change has to be able to ship it, and "shipped" means the node is
//! running the new bytes rather than the record saying so.
//!
//! The component answers with a tag baked in at compile time, so what is actually
//! running is readable from outside. A version field the platform sets is a claim;
//! a string the running code emits is evidence.
//!
//! It really recompiles. `cargo build` runs inside the test with a different
//! `COMP_VERSION_TAG`, and the test refuses to continue if the two builds produce
//! the same bytes — because a cache hit would make everything below it pass while
//! proving nothing.

use std::process::Command;
use std::time::{Duration, Instant};

use comp_reconciler::fleet::{repo_root, Fleet};
use serde_json::{json, Value};

/// Serialises builds.
///
/// Every test here compiles the same crate into the same target directory, and
/// they run on parallel threads — so without this one test replaces the artifact
/// while another is reading it, and the read fails with "No such file". The build
/// AND the read have to be inside the lock: the bytes are only this tag's bytes
/// until somebody else builds.
static BUILD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Build the probe with a tag, and hand back the bytes.
fn build(tag: &str) -> Vec<u8> {
    let _guard = BUILD.lock().unwrap_or_else(|e| e.into_inner());
    let components = repo_root().join("components");
    let out = Command::new("cargo")
        .current_dir(&components)
        .args(["build", "--release", "--target", "wasm32-wasip2", "-p", "version-probe"])
        .env("COMP_VERSION_TAG", tag)
        .output()
        .expect("running cargo");
    assert!(
        out.status.success(),
        "building {tag} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let path = components.join("target/wasm32-wasip2/release/version_probe.wasm");
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

struct Api {
    base: String,
    http: reqwest::blocking::Client,
    token: String,
}

impl Api {
    fn new(base: String) -> Self {
        let http =
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build().unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        while Instant::now() < deadline {
            if http.get(&base).send().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let mut me = Self { base, http, token: String::new() };
        let body = json!({ "email": "ada@version.test", "password": "password123" });
        let _ = me.raw("/api/register", body.clone());
        let v = me.raw("/api/login", body);
        me.token = v["token"].as_str().unwrap_or_default().to_string();
        assert!(!me.token.is_empty(), "could not log in: {v}");
        me
    }

    fn raw(&self, path: &str, body: Value) -> Value {
        self.http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .ok()
            .and_then(|r| r.json().ok())
            .unwrap_or(Value::Null)
    }

    fn post(&self, path: &str, body: Value) -> (u16, Value) {
        match self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
        {
            Ok(r) => (r.status().as_u16(), r.json().unwrap_or(Value::Null)),
            Err(e) => (0, Value::String(format!("transport: {e}"))),
        }
    }

    /// Upload an artifact under an id. The digest is the platform's business —
    /// the caller sends bytes.
    fn get(&self, path: &str) -> Value {
        self.http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .ok()
            .and_then(|r| r.json().ok())
            .unwrap_or(Value::Null)
    }

    // A `digest_of` reading the CATALOGUE's `oci_ref` lived here, claiming to be
    // "the whole question in a version test". Nothing ever called it, and the
    // reason is in the next function's own doc: the manifest digest is what the
    // fleet is actually told to run, and for a fused deployment it is the composed
    // artifact rather than the uploaded component. The catalogue's answer can move
    // without the fleet's doing so, which makes it the wrong number to assert on.

    /// The digest in the CURRENT manifest — what the fleet is actually told to
    /// run. For a fused deployment this is the composed artifact rather than the
    /// uploaded component, which is why it is the number that matters.
    fn manifest_digest(&self, id: &str) -> String {
        self.get(&format!("/api/deployments/{id}/manifests"))["manifest"]["components"][0]["digest"]
            .as_str()
            .unwrap_or("(none)")
            .to_string()
    }

    /// Upload and read the answer, so the caller can learn the content address.
    fn post_bytes(&self, path: &str, wasm: Vec<u8>) -> Value {
        self.http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .body(wasm)
            .send()
            .ok()
            .and_then(|r| r.json().ok())
            .unwrap_or(Value::Null)
    }

    fn upload(&self, id: &str, wasm: Vec<u8>) -> u16 {
        self.http
            .post(format!("{}/api/components?id={id}", self.base))
            .bearer_auth(&self.token)
            .body(wasm)
            .send()
            .unwrap()
            .status()
            .as_u16()
    }
}

/// What the app is serving right now, via the ingress. `None` until it answers.
///
/// The host is `{app}.{org}.{suffix}` — the platform derives it, nobody chooses
/// it, and guessing it wrong reads exactly like the app never starting.
fn tag_served(fleet: &Fleet) -> Option<String> {
    let http =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
    let r = http
        .get(format!("http://127.0.0.1:{}/", fleet.ingress_port))
        .header("host", "ver.ada.test")
        .send()
        .ok()?;
    let v: Value = serde_json::from_str(&r.text().ok()?).ok()?;
    v["tag"].as_str().map(str::to_string)
}

fn wait_for_tag(fleet: &Fleet, want: &str, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if tag_served(fleet).as_deref() == Some(want) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

#[test]
fn a_recompiled_artifact_replaces_the_running_one() {
    // --- the premise, checked before anything depends on it ------------------
    let alpha = build("alpha");
    let beta = build("beta");
    assert_ne!(
        alpha, beta,
        "two builds with different tags produced identical bytes — the rebuild was a \
         cache hit, so everything below this would pass while shipping nothing. That is \
         what `build.rs`'s rerun-if-env-changed is for."
    );
    println!("    alpha {} bytes, beta {} bytes, and they differ", alpha.len(), beta.len());

    let fleet = Fleet::start_with_platform("newversion", 1);
    let api = Api::new(fleet.platform_url());

    // --- ship v1 -------------------------------------------------------------
    assert!(matches!(api.upload("ver", alpha.clone()), 200 | 201), "uploading alpha failed");
    let (code, dep) = api
        .post("/api/deployments", json!({ "name": "ver", "nodes": [{"id": "ver"}], "edges": [] }));
    assert_eq!(code, 201, "deploy failed: {dep}");
    let id = dep["id"].as_str().unwrap().to_string();

    let deadline = Instant::now() + Duration::from_secs(180);
    let mut saved = false;
    let mut why = Value::Null;
    while Instant::now() < deadline && !saved {
        let (code, body) = api.post(&format!("/api/deployments/{id}/save"), json!({}));
        saved = code == 200;
        why = body;
        if !saved {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    assert!(saved, "the first revision never saved: {why}\n{}", fleet.reconciler_log());

    assert!(
        wait_for_tag(&fleet, "alpha", Duration::from_secs(180)),
        "the first build never served — got {:?}\n--- node ---\n{}\n--- reconciler ---\n{}",
        tag_served(&fleet),
        fleet.node_log("n1"),
        fleet.reconciler_log()
    );
    println!("    v1 is serving `alpha`");

    // --- ship v2 over the top ------------------------------------------------
    // Same component id, different bytes. The platform mints a new digest, the
    // revision points at it, and the reconciler has to notice the app it is
    // running is not the app that is wanted.
    let digest_v1 = api.manifest_digest(&id);
    println!("    v1 manifest digest: {digest_v1}");
    assert!(matches!(api.upload("ver", beta.clone()), 200 | 201), "uploading beta failed");

    // Saving is RETRIED, because an upload clears the component's digest: the
    // bytes are staged, and the reconciler's push pass has to put them in the
    // object store and record the address before a revision can reference them
    // (ADR-0006). A single save fired immediately after the upload renders the
    // manifest against the digest that is still there — the old one — and
    // succeeds, which is a revision that ships nothing.
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut served = false;
    while Instant::now() < deadline {
        let (code, _) = api.post(&format!("/api/deployments/{id}/save"), json!({}));
        let _ = code;
        if tag_served(&fleet).as_deref() == Some("beta") {
            served = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }

    assert!(
        served || wait_for_tag(&fleet, "beta", Duration::from_secs(60)),
        "the fleet kept serving the old build after a new one was uploaded and saved — \
         got {:?}. An agent that cannot replace what is running cannot ship anything.\n\
         digest before the upload: {digest_v1}\n\
         digest now:               {}\n\
         manifests: {}\n\
         --- node ---\n{}\n--- reconciler ---\n{}",
        tag_served(&fleet),
        api.manifest_digest(&id),
        api.get(&format!("/api/deployments/{id}/manifests")),
        fleet.node_log("n1"),
        fleet.reconciler_log()
    );
    println!("    v2 is serving `beta` — the recompiled artifact replaced the running one");

    // --- and it STAYS replaced ------------------------------------------------
    // A reconcile pass that flapped between digests would show up here: the old
    // one would come back within an interval or two.
    std::thread::sleep(Duration::from_secs(8));
    assert_eq!(
        tag_served(&fleet).as_deref(),
        Some("beta"),
        "the old build came back — the loop is flapping between two digests rather than \
         converging on the newest revision"
    );

    // The node ran both, which is what "replaced" means: not that the old one was
    // never there, but that it is not there now.
    let log = fleet.node_log("n1");
    assert!(log.contains("started ada/ver/"), "the node never reported starting anything:\n{log}");
    println!("    and it stayed replaced");

    // --- an upgrade that removes an export is refused -------------------------
    //
    // The WIT surface is the WRONG question for "did this change" — two builds
    // differing in a constant have identical surfaces and different bytes, and
    // treating them as the same is the bug this file was written to catch. It is
    // the RIGHT question for "does this break anything": an export that vanished
    // is one something was linking to, or serving on.
    //
    // Uploading the wrong artifact under an existing id is the ordinary way this
    // happens, so that is what is done here — a component that exports a domain
    // interface rather than an HTTP handler.
    let wrong =
        std::fs::read(repo_root().join("components/target/wasm32-wasip2/release/slug.wasm"))
            .expect("run `just build`");
    assert!(matches!(api.upload("ver", wrong), 200 | 201), "uploading the wrong artifact failed");

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut refusal = None;
    while Instant::now() < deadline {
        let (code, body) = api.post(&format!("/api/deployments/{id}/save"), json!({}));
        if code == 409 && body["error"].as_str().unwrap_or_default().contains("no longer exports") {
            refusal = Some(body);
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    let why = refusal.expect(
        "swapping in an artifact that does not export the handler this app serves on was \
         accepted — it would have been caught at start time on a node, as a link failure \
         with no hint that an upload caused it",
    );
    println!("    refused: {}", why["error"].as_str().unwrap_or_default());

    // The fleet is untouched by a refused save: it keeps serving what it had.
    assert_eq!(
        tag_served(&fleet).as_deref(),
        Some("beta"),
        "a refused save changed what was running"
    );

    // And `force` exists because removing an export is sometimes the change.
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut forced = false;
    while Instant::now() < deadline {
        let (code, _) = api.post(&format!("/api/deployments/{id}/save?force=true"), json!({}));
        if code == 200 {
            forced = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    assert!(forced, "`force=true` did not get past the gate, so the gate cannot be overridden");
    println!("    and force=true gets past it");
}

/// What content-addressed staging buys.
///
/// The catalogue row is keyed by `tenant/id` — a NAME — and the bytes used to be
/// staged under that same name. That made an upload destructive: a second build
/// overwrote the first, so two workers pushing different builds of one component
/// raced and the loser's bytes were gone. It also made every upload look like a
/// change, clearing the digest and forcing the whole distribution round again for
/// bytes the fleet already had.
///
/// Staging by content fixes both, and neither needed a lock: identical bytes
/// write identical bytes to the same place, different bytes go somewhere else.
#[test]
fn re_uploading_the_same_bytes_changes_nothing_and_two_builds_do_not_collide() {
    let alpha = build("alpha");
    let beta = build("beta");
    assert_ne!(alpha, beta, "the two builds must differ for this to test anything");

    let fleet = Fleet::start_with_platform("content", 1);
    let api = Api::new(fleet.platform_url());

    // --- the same bytes twice ------------------------------------------------
    assert!(matches!(api.upload("c", alpha.clone()), 200 | 201), "first upload failed");
    let (code, dep) =
        api.post("/api/deployments", json!({ "name": "c", "nodes": [{"id": "c"}], "edges": [] }));
    assert_eq!(code, 201, "deploy failed: {dep}");
    let id = dep["id"].as_str().unwrap().to_string();

    // Get it distributed, so there is a digest to preserve.
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut saved = false;
    while Instant::now() < deadline && !saved {
        let (code, _) = api.post(&format!("/api/deployments/{id}/save"), json!({}));
        saved = code == 200;
        if !saved {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    assert!(saved, "the first revision never saved\n{}", fleet.reconciler_log());
    let digest = api.manifest_digest(&id);
    assert!(digest.starts_with("sha256:"), "no digest after distribution: {digest}");

    // Re-uploading identical bytes must be a no-op. Previously it cleared the
    // digest and forced the whole distribution round again — for bytes the fleet
    // already had, byte for byte.
    let v = api.get("/api/components");
    let _ = v;
    assert!(matches!(api.upload("c", alpha.clone()), 200 | 201), "re-upload failed");
    assert_eq!(
        api.manifest_digest(&id),
        digest,
        "re-uploading identical bytes invalidated the artifact — a retry, a re-run \
         build, or two workers landing on the same output would each cost a full \
         redistribution of bytes nothing has changed"
    );

    // --- two different builds, no lost bytes ---------------------------------
    // Under the old name-keyed staging the second upload overwrote the first's
    // bytes. Under content keys both are staged and neither writer can destroy
    // the other's — which is what lets parallel workers upload without agreeing
    // on anything.
    assert!(matches!(api.upload("c", beta.clone()), 200 | 201), "beta upload failed");
    assert!(matches!(api.upload("c", alpha.clone()), 200 | 201), "alpha re-upload failed");

    // Whichever the pointer ends on, the deployment must still be able to compose
    // — which it can only do if the bytes it names are still there.
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut ok = false;
    while Instant::now() < deadline && !ok {
        let (code, _) = api.post(&format!("/api/deployments/{id}/save"), json!({}));
        ok = code == 200;
        if !ok {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    assert!(
        ok,
        "after two builds were uploaded under one name, the deployment could no longer \
         compose — one upload destroyed the other's bytes\n{}",
        fleet.reconciler_log()
    );

    println!("    identical bytes are a no-op, and two builds coexist under one name");
}

/// A deployment pinned to a digest does not move when the name does.
///
/// This is what the whole identity rework is FOR. A bare name is whatever was
/// uploaded last, which is fine until somebody uploads while you are deploying —
/// and then a deployment that looked reproducible quietly is not. Naming the
/// bytes makes it a fact rather than a hope.
#[test]
fn a_deployment_pinned_to_a_digest_ignores_a_later_upload() {
    let alpha = build("alpha");
    let beta = build("beta");
    assert_ne!(alpha, beta);

    let fleet = Fleet::start_with_platform("pinned", 1);
    let api = Api::new(fleet.platform_url());

    // Upload alpha under a tag, and learn its content address.
    let v = api.post_bytes("/api/components?id=p&tag=v1", alpha.clone());
    let sha = v["content"].as_str().unwrap_or_default().to_string();
    assert!(!sha.is_empty(), "the upload should report the content it staged: {v}");

    // Deploy PINNED to those bytes.
    let (code, dep) = api.post(
        "/api/deployments",
        json!({ "name": "p", "nodes": [{"id": format!("p@sha256:{sha}")}], "edges": [] }),
    );
    assert_eq!(code, 201, "a pinned deployment should be accepted: {dep}");
    let id = dep["id"].as_str().unwrap().to_string();

    let deadline = Instant::now() + Duration::from_secs(180);
    let mut saved = false;
    while Instant::now() < deadline && !saved {
        let (code, _) = api.post(&format!("/api/deployments/{id}/save"), json!({}));
        saved = code == 200;
        if !saved {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    assert!(saved, "the pinned revision never saved\n{}", fleet.reconciler_log());
    let pinned_digest = api.manifest_digest(&id);

    // Now move the name. Under a bare reference this is what would change what
    // the deployment runs.
    assert!(matches!(api.upload("p", beta.clone()), 200 | 201), "uploading beta failed");

    // Saving again must render the SAME artifact: the deployment named bytes, and
    // those bytes did not change just because the name did.
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut re_saved = false;
    while Instant::now() < deadline && !re_saved {
        let (code, _) = api.post(&format!("/api/deployments/{id}/save"), json!({}));
        re_saved = code == 200;
        if !re_saved {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    assert!(re_saved, "the pinned deployment could not be saved after the name moved");
    assert_eq!(
        api.manifest_digest(&id),
        pinned_digest,
        "a deployment pinned to a digest followed the NAME instead — pinning that does \
         not pin is worse than no pinning, because it looks reproducible"
    );

    // And a digest nobody ever uploaded is refused at SAVE, which is where a
    // deployment stops being a draft and becomes something a fleet is told to
    // run. Creating it is not the moment — an author is still editing.
    let (code, ghost) = api.post(
        "/api/deployments",
        json!({ "name": "ghost", "nodes": [{"id": "p@sha256:deadbeef"}], "edges": [] }),
    );
    assert_eq!(code, 201, "a draft may name anything: {ghost}");
    let ghost_id = ghost["id"].as_str().unwrap().to_string();
    let (code, body) = api.post(&format!("/api/deployments/{ghost_id}/save"), json!({}));
    assert!(
        code >= 400 && body["error"].as_str().unwrap_or_default().contains("no staged bytes"),
        "a reference to bytes nobody has must be refused where the author is standing, \
         not at start time on a node: {code} {body}"
    );

    println!("    pinned to {}, and it stayed pinned", &sha[..12.min(sha.len())]);
}
