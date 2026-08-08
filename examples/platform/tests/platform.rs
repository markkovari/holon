//! E2E for the deployment platform (docs/adr/) as ONE composed wasm HTTP component
//! (platform-domain + auth-guard + policy + records + blob + quota + wit-reflect)
//! on the native host, with the real applier alongside it in **validate-only**
//! mode — it builds no Kubernetes client, so this test needs no cluster and cannot
//! touch one.
//!
//! What it proves, in ADR order:
//!
//!   0009  sign-in works, and a session is required for everything
//!   0006  a component with no registry digest CANNOT be deployed — the platform
//!         refuses rather than rendering a tag
//!   0005  both strategies render, and the planner refuses a strategy the graph
//!         cannot support
//!   0002  the workload lands in the tenant's own namespace, derived not supplied
//!   0008  the isolation stamp is present: tenant bucket, fail-closed egress
//!   0003  the applier re-validates and rejects anything aimed elsewhere
//!   0007  another tenant cannot see or use a private component
//!   0004  a save creates a revision, and the applier can list what to re-apply
//!   0014  each application gets its OWN host — private data NATS, own engine, own
//!         endpoint — so two apps of one tenant share no storage and no compute, and
//!         `wasi:keyvalue` is bindable again

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const PLATFORM: &str = "127.0.0.1:3057";
const APPLIER: &str = "127.0.0.1:8091";
const SECRET: &str = "e2e-secret";

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}
fn rel() -> PathBuf {
    root().join("components/target/wasm32-wasip2/release")
}

// ---- http -------------------------------------------------------------------

fn req(method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, Value) {
    let url = format!("http://{PLATFORM}{path}");
    let mut r = match method {
        "GET" => ureq::get(&url),
        "POST" => ureq::post(&url),
        "DELETE" => ureq::delete(&url),
        m => panic!("method {m}"),
    };
    if let Some(t) = token {
        r = r.set("authorization", &format!("bearer {t}"));
    }
    let sent = match body {
        Some(b) => r.set("content-type", "application/json").send_string(&b.to_string()),
        None => r.call(),
    };
    match sent {
        Ok(resp) => (resp.status(), json_of(resp)),
        Err(ureq::Error::Status(s, resp)) => (s, json_of(resp)),
        Err(e) => panic!("{method} {path}: {e}"),
    }
}
fn json_of(resp: ureq::Response) -> Value {
    let text = resp.into_string().unwrap_or_default();
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}
fn text_of(path: &str, token: &str) -> String {
    ureq::get(&format!("http://{PLATFORM}{path}"))
        .set("authorization", &format!("bearer {token}"))
        .call()
        .expect("manifests")
        .into_string()
        .unwrap()
}

fn upload(token: &str, stem: &str) -> Value {
    let id = stem.replace('_', "-");
    let bytes = std::fs::read(rel().join(format!("{stem}.wasm")))
        .unwrap_or_else(|e| panic!("{stem}.wasm: {e} — run `just build`"));
    let resp = ureq::post(&format!("http://{PLATFORM}/api/components?id={id}"))
        .set("authorization", &format!("bearer {token}"))
        .set("content-type", "application/wasm")
        .send_bytes(&bytes);
    match resp {
        Ok(r) => {
            assert_eq!(r.status(), 201);
            json_of(r)
        }
        Err(e) => panic!("upload {id}: {e}"),
    }
}

/// The seam a push step calls once an artifact is in the registry (ADR-0006).
fn record_push(key: &str, digest: &str) -> u16 {
    let r = ureq::post(&format!("http://{PLATFORM}/api/internal/pushed"))
        .set("x-platform-secret", SECRET)
        .set("content-type", "application/json")
        .send_string(&json!({ "key": key, "digest": digest }).to_string());
    match r {
        Ok(resp) => resp.status(),
        Err(ureq::Error::Status(s, _)) => s,
        Err(e) => panic!("pushed: {e}"),
    }
}

/// Refuse to run against someone else's process. Every e2e in this repo assumes its
/// port is free, and when it is not the test silently talks to a stranger — which has
/// produced two confusing failures already (a passkey run asserting against a
/// tailnet-configured host, and a platform run finding `ada` already registered).
fn require_port_free(addr: &str, what: &str) {
    if std::net::TcpStream::connect_timeout(
        &addr.parse().expect("addr"),
        Duration::from_millis(300),
    )
    .is_ok()
    {
        panic!(
            "{addr} is already in use, so this test would run against that process instead of its own {what}. \
             Stop it first (e.g. `pkill -f comp-host`, `pkill -f release/applier`)."
        );
    }
}

fn start_all() -> (Kill, Kill) {
    require_port_free(PLATFORM, "platform");
    require_port_free(APPLIER, "applier");
    // The applier first — validate-only, so no Kubernetes client is ever built.
    let applier_bin = root().join("applier/target/release/applier");
    assert!(applier_bin.exists(), "applier not built (run `just e2e-platform`)");
    let applier = Command::new(&applier_bin)
        .args(["--addr", APPLIER, "--secret", SECRET, "--validate-only"])
        .spawn()
        .expect("spawn applier");
    let applier = Kill(applier);
    for _ in 0..100 {
        if ureq::get(&format!("http://{APPLIER}/healthz")).call().is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let host = root().join("host/target/release/comp-host");
    let component = root().join("components/target/platform_domain.composed.wasm");
    assert!(component.exists(), "composed wasm missing (just compose-platform)");
    let platform = Command::new(&host)
        .args(["--component", component.to_str().unwrap(), "--addr", PLATFORM, "--kv", "memory"])
        .env("VET_TENANT", "platform")
        .env("CFG_APPLIER_URL", format!("http://{APPLIER}"))
        .env("CFG_APPLIER_SECRET", SECRET)
        .env("CFG_REGISTRY", "registry.platform.svc.cluster.local:5000")
        .env("CFG_CLUSTER_SUFFIX", "svc.cluster.local")
        .spawn()
        .expect("spawn comp-host");
    let platform = Kill(platform);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&format!("http://{PLATFORM}/")).call() {
            if r.status() == 200 {
                return (applier, platform);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("platform did not start");
}

const DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const DIGEST2: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

#[test]
#[ignore = "drives the deleted applier + asserts rendered YAML. The lane it tested is \
gone (ADR-0021/0022); the harness below — require_port_free, Kill, req, upload — is \
worth keeping and the assertions need rewriting against the JSON manifest. Until then \
the end-to-end path is exercised by hand, per ADR-0025."]
fn platform_signs_in_renders_and_applies() {
    let (_applier, _platform) = start_all();

    // ===== 0009: identity ==================================================
    let (code, _) = req("POST", "/api/register", None, Some(json!({ "email": "ada@acme.dev", "password": "correct-horse-battery" })));
    assert_eq!(code, 201, "register");
    let (code, login) = req("POST", "/api/login", None, Some(json!({ "email": "ada@acme.dev", "password": "correct-horse-battery" })));
    assert_eq!(code, 200, "{login}");
    let token = login["token"].as_str().unwrap().to_string();
    assert_eq!(login["tenant"], "ada", "tenant derived from the account");

    let (code, me) = req("GET", "/api/me", Some(&token), None);
    assert_eq!(code, 200);
    assert_eq!(me["tenant"], "ada");
    // Everything needs a session.
    assert_eq!(req("GET", "/api/components", None, None).0, 401);
    assert_eq!(req("GET", "/api/deployments", None, None).0, 401);

    // ===== catalog: reflection is validation ===============================
    let mesh = upload(&token, "mesh_domain");
    assert_eq!(mesh["visibility"], "private");
    assert_eq!(mesh["oci_ref"], "", "not deployable until pushed");
    assert_eq!(mesh["surface"]["imports"].as_array().unwrap().len(), 3, "reflected: {}", mesh["surface"]);
    for stem in ["record_store", "resilience", "proxy_route"] {
        upload(&token, stem);
    }
    let (_, list) = req("GET", "/api/components", Some(&token), None);
    let rows = list["components"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|r| r["deployable"] == json!(false)), "none pushed yet");

    // A core module is refused, so a bad upload never becomes a catalog row.
    let bad = ureq::post(&format!("http://{PLATFORM}/api/components?id=junk"))
        .set("authorization", &format!("bearer {token}"))
        .set("content-type", "application/wasm")
        .send_bytes(b"\0asm\x01\0\0\0");
    assert!(matches!(bad, Err(ureq::Error::Status(422, _))), "a core module is not a component");

    // ===== 0005 + 0006: a deployment cannot ship an unpinned component =====
    let nodes = json!(["mesh-domain", "record-store", "resilience", "proxy-route"]);
    let edges = json!([
        { "plug": "record-store", "socket": "mesh-domain", "iface": "records:store/store@0.1.0" },
        { "plug": "resilience",   "socket": "mesh-domain", "iface": "resilience:breaker/breaker@0.1.0" },
        { "plug": "proxy-route",  "socket": "mesh-domain", "iface": "proxy:route/router@0.1.0" },
    ]);
    let (code, created) = req(
        "POST",
        "/api/deployments",
        Some(&token),
        Some(json!({ "name": "api", "strategy": "linked", "nodes": nodes, "edges": edges })),
    );
    assert_eq!(code, 201, "{created}");
    let id = created["id"].as_str().unwrap().to_string();

    let (code, refused) = req("POST", &format!("/api/deployments/{id}/save"), Some(&token), Some(json!({})));
    assert_eq!(code, 409, "no digest yet: {refused}");
    assert!(refused["error"].as_str().unwrap().contains("not in the registry yet"), "{refused}");

    // ===== record pushes (the seam), then save for real ====================
    assert_eq!(record_push("ada/mesh-domain", DIGEST), 200);
    for c in ["record-store", "resilience", "proxy-route"] {
        assert_eq!(record_push(&format!("ada/{c}"), DIGEST2), 200);
    }
    // The internal endpoint is secret-gated.
    let unauth = ureq::post(&format!("http://{PLATFORM}/api/internal/pushed"))
        .set("content-type", "application/json")
        .send_string(&json!({ "key": "ada/mesh-domain", "digest": DIGEST }).to_string());
    assert!(matches!(unauth, Err(ureq::Error::Status(401, _))));

    let (code, saved) = req("POST", &format!("/api/deployments/{id}/save"), Some(&token), Some(json!({})));
    assert_eq!(code, 200, "linked save: {saved}");
    assert_eq!(saved["revision"], 1);
    assert_eq!(saved["namespace"], "tenant-ada", "0002: derived namespace");
    // The applier validated every object and said which.
    let applied = saved["applier"]["applied"].as_array().unwrap();
    assert_eq!(saved["applier"]["validated_only"], true);
    assert!(applied.iter().any(|a| a == "WorkloadDeployment/api"), "{applied:?}");
    assert!(applied.iter().any(|a| a == "Service/api"), "{applied:?}");

    // ===== the rendered manifests are the product ==========================
    let yaml = text_of(&format!("/api/deployments/{id}/manifests"), &token);
    assert!(yaml.contains("kind: WorkloadDeployment"));
    assert!(yaml.contains("namespace: tenant-ada"));
    assert!(yaml.contains("platform.comp/strategy: linked"));
    // 0005 linked: every component present, edges absent (the runtime links them).
    assert_eq!(yaml.matches("          poolSize:").count(), 4, "{yaml}");
    assert!(!yaml.contains("records:store/store"), "an edge must not appear: {yaml}");
    // 0006: digests, never tags.
    assert!(yaml.contains(&format!("@{DIGEST}")));
    assert!(!yaml.contains(":0.1.0\n"), "no tag anywhere: {yaml}");
    // 0005: one hostInterfaces entry per interface, never merged.
    assert!(yaml.contains("interfaces: [store]"), "{yaml}");
    assert!(!yaml.contains("interfaces: [store, atomics]"));
    // 0014: the app's own host, which is what makes keyvalue safe to bind. Its data
    // plane must be on loopback — a Service URL here would put every app back on one
    // shared bus, which is exactly the leak ADR-0012 measured.
    assert!(yaml.contains("kind: Deployment"), "the app's host: {yaml}");
    assert!(yaml.contains("--data-nats-url=nats://127.0.0.1:4222"), "{yaml}");
    assert!(yaml.contains("      environment: app-ada-api\n"), "workload pinned to it: {yaml}");
    assert!(yaml.contains("kind: PersistentVolumeClaim"), "durable storage: {yaml}");
    // The namespace and its guardrails travel with it, or the host pod has nowhere to
    // run and no route to the control plane.
    assert!(yaml.contains("kind: Namespace"), "{yaml}");
    assert!(yaml.contains("kind: NetworkPolicy"), "{yaml}");
    // 0008/0012: still no keyvalue bucket key, because nothing reads one. The
    // boundary is the private bus, not a manifest field.
    assert!(!yaml.contains("bucket: t-ada"), "must not fake kv isolation: {yaml}");
    assert!(yaml.contains("allowedHosts:"), "fail-closed egress is explicit: {yaml}");
    assert!(yaml.contains("permits no egress"), "this plan has none: {yaml}");

    // ===== 0005: the other strategy, and a refusal =========================
    // `fused` renders exactly one component — the composed root.
    let (code, fused) = req(
        "POST",
        &format!("/api/deployments/{id}/save"),
        Some(&token),
        Some(json!({ "strategy": "fused" })),
    );
    assert_eq!(code, 200, "fused save: {fused}");
    assert_eq!(fused["revision"], 2, "a save is a revision (0004)");
    let yaml = text_of(&format!("/api/deployments/{id}/manifests"), &token);
    assert_eq!(yaml.matches("          poolSize:").count(), 1, "fused is one artifact: {yaml}");
    assert!(yaml.contains("platform.comp/strategy: fused"));
    // ...and it still needs every host interface the graph did.
    assert!(yaml.contains("interfaces: [store]"), "{yaml}");

    // An unsatisfied graph is refused with the interface named (0005).
    let (code, gapped) = req(
        "POST",
        "/api/deployments",
        Some(&token),
        Some(json!({ "name": "gapped", "strategy": "fused", "nodes": ["mesh-domain"], "edges": [] })),
    );
    assert_eq!(code, 201, "{gapped}");
    let gid = gapped["id"].as_str().unwrap();
    let (code, err) = req("POST", &format!("/api/deployments/{gid}/save"), Some(&token), Some(json!({})));
    assert_eq!(code, 422);
    assert!(err["error"].as_str().unwrap().contains("still needs"), "{err}");

    // An unknown strategy never reaches the renderer.
    let (code, _) = req(
        "POST",
        "/api/deployments",
        Some(&token),
        Some(json!({ "name": "weird", "strategy": "host-plugins", "nodes": ["mesh-domain"] })),
    );
    assert_eq!(code, 422, "strategy must be fused|linked");

    // ===== 0004: what the applier re-applies ===============================
    let revisions = ureq::get(&format!("http://{PLATFORM}/api/internal/revisions"))
        .set("x-platform-secret", SECRET)
        .call()
        .expect("revisions");
    let revisions = json_of(revisions);
    let list = revisions["revisions"].as_array().unwrap();
    assert_eq!(list.len(), 1, "one current revision per deployment: {list:?}");
    assert_eq!(list[0]["revision"], 2, "the latest, not every one");
    assert_eq!(list[0]["namespace"], "tenant-ada");

    // ===== 0007: another tenant sees nothing of ours =======================
    req("POST", "/api/register", None, Some(json!({ "email": "eve@globex.dev", "password": "another-long-one" })));
    let (_, eve_login) = req("POST", "/api/login", None, Some(json!({ "email": "eve@globex.dev", "password": "another-long-one" })));
    let eve = eve_login["token"].as_str().unwrap().to_string();
    let (_, eve_list) = req("GET", "/api/components", Some(&eve), None);
    assert!(eve_list["components"].as_array().unwrap().is_empty(), "private stays private: {eve_list}");
    // ...and cannot deploy them either.
    let (code, _) = req(
        "POST",
        "/api/deployments",
        Some(&eve),
        Some(json!({ "name": "steal", "strategy": "fused", "nodes": ["mesh-domain"] })),
    );
    assert_eq!(code, 201, "creating a draft is fine");
    let (_, eve_deployments) = req("GET", "/api/deployments", Some(&eve), None);
    let sid = eve_deployments["deployments"][0]["id"].as_str().unwrap().to_string();
    // ADR-0012's gate is LIFTED (ADR-0014): a second tenant no longer shares storage
    // with the first, because neither shares a host. Eve's save fails on the one
    // thing that should still stop it — she cannot use ada's private component.
    let (code, denied) = req("POST", &format!("/api/deployments/{sid}/save"), Some(&eve), Some(json!({})));
    assert_eq!(code, 422, "{denied}");
    let msg = denied["error"].as_str().unwrap();
    assert!(msg.contains("not visible to you"), "refused for ownership, not storage: {msg}");
    assert!(!msg.contains("adr/0012"), "the storage gate is lifted (0014): {msg}");

    // Eve cannot read ada's deployment at all.
    assert_eq!(req("GET", &format!("/api/deployments/{id}"), Some(&eve), None).0, 404);
    assert_eq!(req("GET", &format!("/api/deployments/{id}/manifests"), Some(&eve), None).0, 404);

    // ===== 0007: public needs a signature, so it is refused honestly =======
    let (code, pubattempt) = req(
        "POST",
        "/api/components/publish",
        Some(&token),
        Some(json!({ "id": "resilience", "visibility": "public" })),
    );
    assert_eq!(code, 501, "{pubattempt}");
    assert!(pubattempt["error"].as_str().unwrap().contains("signing"), "{pubattempt}");
    // org works today.
    let (code, orgd) = req(
        "POST",
        "/api/components/publish",
        Some(&token),
        Some(json!({ "id": "resilience", "visibility": "org" })),
    );
    assert_eq!(code, 200, "{orgd}");
    assert_eq!(orgd["visibility"], "org");

    // ===== 0014: a second app of the same tenant shares nothing =============
    // The level the cluster test failed at before (ADR-0012 proved two *tenants*
    // leaked; two *apps* of one tenant leaked for the same reason). Same components,
    // same namespace, same everything except the app — so if any isolation-bearing
    // name were derived from the tenant alone, it would collide here.
    let (code, second) = req(
        "POST",
        "/api/deployments",
        Some(&token),
        Some(json!({ "name": "billing", "strategy": "linked", "nodes": nodes, "edges": edges })),
    );
    assert_eq!(code, 201, "{second}");
    let bid = second["id"].as_str().unwrap().to_string();
    let (code, saved2) = req("POST", &format!("/api/deployments/{bid}/save"), Some(&token), Some(json!({})));
    assert_eq!(code, 200, "second app saves: {saved2}");

    let a = text_of(&format!("/api/deployments/{id}/manifests"), &token);
    let b = text_of(&format!("/api/deployments/{bid}/manifests"), &token);
    // Separate hosts, separate storage, separate scheduling target.
    assert!(a.contains("environment: app-ada-api") && b.contains("environment: app-ada-billing"), "{b}");
    assert!(a.contains("app-ada-api-host") && b.contains("app-ada-billing-host"));
    assert!(a.contains("app-ada-api-data") && b.contains("app-ada-billing-data"), "separate claims");
    assert!(!b.contains("app-ada-api"), "no name from the other app appears: {b}");
    // Both bind keyvalue, which is only sound because neither shares a bus.
    assert!(a.contains("interfaces: [store]") && b.contains("interfaces: [store]"));
    assert!(a.contains("--data-nats-url=nats://127.0.0.1:4222"));
    // The applier accepted the host pod, which means its image allow-list matched.
    let applied = saved2["applier"]["applied"].as_array().unwrap();
    assert!(applied.iter().any(|x| x == "Deployment/app-ada-billing-host"), "{applied:?}");
    assert!(applied.iter().any(|x| x == "PersistentVolumeClaim/app-ada-billing-data"), "{applied:?}");
    // Every object carries the env, which is what makes deleting an app one selector.
    assert!(b.contains("platform.comp/env: app-ada-billing"), "{b}");

    // ===== 0015: deleting an app removes its footprint, then its records =====
    // Until this existed there was no delete path at all, so an app's host pod, claim
    // and self-registered `Host` outlived it (measured on a cluster).
    // An unconfirmed delete is refused: it would destroy the app's storage.
    let (code, unconfirmed) = req("DELETE", &format!("/api/deployments/{bid}"), Some(&token), None);
    assert_eq!(code, 428, "{unconfirmed}");
    assert!(unconfirmed["error"].as_str().unwrap().contains("?confirm=billing"), "{unconfirmed}");
    // Naming the WRONG app does not count either.
    assert_eq!(
        req("DELETE", &format!("/api/deployments/{bid}?confirm=api"), Some(&token), None).0,
        428,
        "the token must name the app being deleted"
    );

    let (code, deleted) = req("DELETE", &format!("/api/deployments/{bid}?confirm=billing"), Some(&token), None);
    assert_eq!(code, 200, "{deleted}");
    assert_eq!(deleted["env"], "app-ada-billing");
    assert_eq!(deleted["applier"]["validated_only"], true, "{deleted}");
    assert_eq!(deleted["applier"]["env"], "app-ada-billing");

    // The deployment and its revisions are gone; the tenant's other app is untouched.
    assert_eq!(req("GET", &format!("/api/deployments/{bid}"), Some(&token), None).0, 404);
    assert_eq!(req("GET", &format!("/api/deployments/{id}"), Some(&token), None).0, 200);
    let revs = json_of(
        ureq::get(&format!("http://{PLATFORM}/api/internal/revisions"))
            .set("x-platform-secret", SECRET)
            .call()
            .expect("revisions"),
    );
    let list = revs["revisions"].as_array().cloned().unwrap_or_default();
    assert!(
        list.iter().all(|r| r["deployment"] != json!(bid)),
        "a deleted app must leave no revision — the reaper reads this to decide what is live: {list:?}"
    );
    // ...and what remains still carries its env, or the reaper would call it an orphan.
    assert!(list.iter().all(|r| r["env"].as_str().is_some_and(|e| e.starts_with("app-"))), "{list:?}");

    // Another tenant cannot delete it.
    let (code, _) = req("DELETE", &format!("/api/deployments/{id}?confirm=api"), Some(&eve), None);
    assert_eq!(code, 404, "eve must not be able to delete ada's app");
}
/// The applier's own boundary, exercised over HTTP rather than as a unit test:
/// it refuses a payload aimed at a namespace the request does not name.
#[test]
#[ignore = "drives the deleted applier + asserts rendered YAML. The lane it tested is \
gone (ADR-0021/0022); the harness below — require_port_free, Kill, req, upload — is \
worth keeping and the assertions need rewriting against the JSON manifest. Until then \
the end-to-end path is exercised by hand, per ADR-0025."]
fn applier_refuses_a_cross_namespace_payload() {
    let applier_bin = root().join("applier/target/release/applier");
    assert!(applier_bin.exists(), "applier not built");
    let child = Command::new(&applier_bin)
        .args(["--addr", "127.0.0.1:8092", "--secret", SECRET, "--validate-only"])
        .spawn()
        .expect("spawn applier");
    let _guard = Kill(child);
    for _ in 0..100 {
        if ureq::get("http://127.0.0.1:8092/healthz").call().is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let manifest = "apiVersion: runtime.wasmcloud.dev/v1alpha1\nkind: WorkloadDeployment\nmetadata:\n  name: api\n  namespace: tenant-victim\n";
    let post = |body: Value, secret: Option<&str>| {
        let mut r = ureq::post("http://127.0.0.1:8092/apply").set("content-type", "application/json");
        if let Some(s) = secret {
            r = r.set("x-platform-secret", s);
        }
        match r.send_string(&body.to_string()) {
            Ok(resp) => (resp.status(), json_of(resp)),
            Err(ureq::Error::Status(s, resp)) => (s, json_of(resp)),
            Err(e) => panic!("apply: {e}"),
        }
    };

    // No secret: refused before anything is parsed.
    assert_eq!(post(json!({ "namespace": "tenant-a", "manifests": manifest }), None).0, 401);

    // The object says tenant-victim, the request says tenant-attacker.
    let (code, body) = post(
        json!({ "namespace": "tenant-attacker", "manifests": manifest }),
        Some(SECRET),
    );
    assert_eq!(code, 422);
    assert!(body["detail"].as_str().unwrap().contains("namespaced into"), "{body}");

    // A namespace outside the platform's prefix is refused whatever it contains.
    let (code, body) = post(json!({ "namespace": "kube-system", "manifests": manifest }), Some(SECRET));
    assert_eq!(code, 422);
    assert!(body["detail"].as_str().unwrap().contains("does not start with"), "{body}");

    // And a kind the platform has no business creating.
    let secret_obj = "apiVersion: v1\nkind: Secret\nmetadata:\n  name: creds\n  namespace: tenant-a\n";
    let (code, body) = post(json!({ "namespace": "tenant-a", "manifests": secret_obj }), Some(SECRET));
    assert_eq!(code, 422);
    assert!(body["detail"].as_str().unwrap().contains("allow-listed"), "{body}");
}
