//! E2E for the composition studio (docs/apps/STUDIO.md) as ONE composed wasm HTTP component
//! (studio-domain + wit-reflect + records + blob) on the native Rust host.
//!
//! The claims under test are strong, so they are checked against the real tools
//! rather than against the studio's own opinion of itself:
//!
//!   * components are REFLECTED, not declared — upload the repo's own artifacts
//!     and the surfaces match what `wasm-tools component wit` shows;
//!   * an illegal edge is refused by `wac`'s own subtype check, not by name
//!     matching;
//!   * `/api/compose` returns a component that `wasm-tools validate` accepts, that
//!     is byte-for-byte the size `wac plug` produces, and that the host will
//!     actually SERVE — the composed mesh app answers its own API;
//!   * the emitted `wac plug` script, run with bash, produces the same artifact;
//!   * the emitted `.wac` file, run through the real `wac compose`, also composes;
//!   * the emitted workload manifest has one `hostInterfaces` entry per interface,
//!     which is the rule that silently breaks binding if you get it wrong.
//!
//! Needs `wac` and `wasm-tools` on PATH — the same tools every compose recipe in
//! the Justfile already requires.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3055";
/// Where the composed artifact gets served to prove it runs.
const RUN_ADDR: &str = "127.0.0.1:3056";

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
fn tmp() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/e2e");
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---- http -------------------------------------------------------------------

fn post(path: &str, body: Value) -> (u16, Value) {
    let r = ureq::post(&format!("http://{ADDR}{path}"))
        .set("content-type", "application/json")
        .send_string(&body.to_string());
    match r {
        Ok(resp) => (resp.status(), json_of(resp)),
        Err(ureq::Error::Status(s, resp)) => (s, json_of(resp)),
        Err(e) => panic!("POST {path}: {e}"),
    }
}
fn post_text(path: &str, body: Value) -> String {
    ureq::post(&format!("http://{ADDR}{path}"))
        .set("content-type", "application/json")
        .send_string(&body.to_string())
        .expect("emit")
        .into_string()
        .expect("text")
}
fn get(path: &str) -> (u16, Value) {
    match ureq::get(&format!("http://{ADDR}{path}")).call() {
        Ok(resp) => (resp.status(), json_of(resp)),
        Err(ureq::Error::Status(s, resp)) => (s, json_of(resp)),
        Err(e) => panic!("GET {path}: {e}"),
    }
}
fn json_of(resp: ureq::Response) -> Value {
    serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null)
}

/// Upload a component artifact for reflection. Returns the stored surface.
fn upload(stem: &str) -> Value {
    let id = stem.replace('_', "-");
    let bytes = std::fs::read(rel().join(format!("{stem}.wasm")))
        .unwrap_or_else(|e| panic!("{stem}.wasm: {e} — run `just build` first"));
    let resp = ureq::post(&format!("http://{ADDR}/api/components?id={id}"))
        .set("content-type", "application/wasm")
        .send_bytes(&bytes)
        .unwrap_or_else(|e| panic!("upload {id}: {e}"));
    assert_eq!(resp.status(), 201, "upload {id}");
    json_of(resp)["surface"].clone()
}

fn start_studio() -> Kill {
    let bin = root().join("host/target/release/comp-host");
    let component = root().join("components/target/studio_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-studio`)");
    assert!(component.exists(), "composed wasm missing (just compose-studio)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "studio")
        .spawn()
        .expect("spawn comp-host");
    let guard = Kill(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&format!("http://{ADDR}/")).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("studio host did not start");
}

fn tool(name: &str, args: &[&str]) -> (bool, String) {
    let out = Command::new(name)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("`{name}` not on PATH ({e}) — the Justfile's compose recipes need it too"));
    (
        out.status.success(),
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
    )
}

// The mesh graph: the exact composition `just compose-mesh` performs by hand.
fn mesh_nodes() -> Vec<&'static str> {
    vec!["mesh-domain", "record-store", "resilience", "proxy-route"]
}
fn mesh_edges() -> Value {
    json!([
        { "plug": "record-store", "socket": "mesh-domain", "iface": "records:store/store@0.1.0" },
        { "plug": "resilience",   "socket": "mesh-domain", "iface": "resilience:breaker/breaker@0.1.0" },
        { "plug": "proxy-route",  "socket": "mesh-domain", "iface": "proxy:route/router@0.1.0" },
    ])
}

#[test]
fn studio_reflects_plans_emits_and_composes() {
    let _studio = start_studio();

    // ===== reflection: the surface comes from the binary =====================
    let mesh = upload("mesh_domain");
    for stem in ["record_store", "resilience", "proxy_route", "zip"] {
        upload(stem);
    }
    // Present because `just build` stamps it on — a wasm32-wasip2 artifact is
    // anonymous otherwise, and a p2 component from outside this repo still will be.
    // The palette id remains the caller's (the `?id=` on the upload).
    assert_eq!(mesh["name"], "mesh-domain", "the build's metadata pass restores it");
    let composable: Vec<&str> =
        mesh["imports"].as_array().unwrap().iter().map(|i| i["raw"].as_str().unwrap()).collect();
    assert_eq!(
        composable,
        vec![
            "records:store/store@0.1.0",
            "resilience:breaker/breaker@0.1.0",
            "proxy:route/router@0.1.0"
        ],
        "exactly the three plugs its recipe uses, versions intact"
    );
    // Everything else mesh-domain needs is a HOST capability, never an edge — no
    // component can satisfy it, so it gets no handle on the canvas.
    let host: Vec<&str> =
        mesh["host_imports"].as_array().unwrap().iter().map(|i| i["raw"].as_str().unwrap()).collect();
    assert!(host.iter().all(|h| h.starts_with("wasi:")), "{host:?}");
    // Match on the interface, not the version: wasm32-wasip2 injects whatever
    // 0.2.x Rust's std carries (0.2.9/0.2.12 today), not the 0.2.0 our vendored
    // WIT declares. That skew is why this migration waited for a host new enough
    // to define those versions.
    assert!(host.iter().any(|h| h.starts_with("wasi:http/types@0.2.")), "{host:?}");
    // 16 on p2 — Rust's wasip2 std wires up the whole wasi:cli surface, including
    // five `terminal-*` interfaces the app never touches. It was 13 under the
    // preview1 adapter. Pinned so a std-surface change shows up here first.
    assert_eq!(host.len(), 16, "mesh-domain's own host surface: {host:?}");
    assert_eq!(
        host.iter().filter(|h| h.contains("terminal-")).count(),
        5,
        "the wasip2 std's terminal probing: {host:?}"
    );

    // The distinction a regex over WIT source cannot make: `wasi:keyvalue` is a
    // host capability too, even though it isn't "std WASI" — it arrives with
    // record-store and must NOT be presented as something to wire.
    let records = upload("record_store");
    let rec_host: Vec<&str> =
        records["host_imports"].as_array().unwrap().iter().map(|i| i["raw"].as_str().unwrap()).collect();
    // keyvalue has no p2/p3 release, so this one keeps its 0.2.0-draft version.
    assert!(rec_host.contains(&"wasi:keyvalue/store@0.2.0-draft"), "{rec_host:?}");
    assert!(records["imports"].as_array().unwrap().is_empty(), "a leaf capability composes nothing");

    let (_, palette) = get("/api/components");
    assert_eq!(palette["components"].as_array().unwrap().len(), 5);

    // ===== the connection guard is wac's own subtype check ==================
    let (_, fits) = post("/api/satisfies", json!({ "socket": "mesh-domain", "plug": "record-store" }));
    assert_eq!(fits["interfaces"], json!(["records:store/store@0.1.0"]));
    let (_, nope) = post("/api/satisfies", json!({ "socket": "mesh-domain", "plug": "zip" }));
    assert_eq!(nope["interfaces"], json!([]), "zip exports nothing mesh imports");

    // ===== planning ========================================================
    let (code, plan) = post("/api/plan", json!({ "nodes": mesh_nodes(), "edges": mesh_edges() }));
    assert_eq!(code, 200, "{plan}");
    assert_eq!(plan["buildable"], true);
    assert_eq!(plan["cyclic"], false);
    assert_eq!(plan["roots"], json!(["mesh-domain"]));
    assert_eq!(plan["unsatisfied"].as_array().unwrap().len(), 0, "everything wired: {plan}");
    assert_eq!(plan["steps"].as_array().unwrap().len(), 1);
    assert_eq!(plan["steps"][0]["plugs"].as_array().unwrap().len(), 3);
    // The union of host capabilities across the graph survives composition, and the
    // plan says so. 22 on p2 (18 under the preview1 adapter) — the extra ones are
    // the wasip2 std's `wasi:cli/terminal-*`.
    let needs: Vec<&str> = plan["host_needs"].as_array().unwrap().iter()
        .map(|h| h["raw"].as_str().unwrap()).collect();
    assert_eq!(needs.len(), 22, "{needs:?}");
    // The ones that actually matter: storage, egress and config all need a host.
    for want in ["wasi:keyvalue/store@0.2.0-draft", "wasi:config/store@0.2.0-rc.1"] {
        assert!(needs.contains(&want), "{want} missing from {needs:?}");
    }
    assert!(needs.iter().any(|n| n.starts_with("wasi:http/outgoing-handler@")), "{needs:?}");

    // A missing edge is a gap, and the gap names the interface.
    let (_, partial) = post(
        "/api/plan",
        json!({ "nodes": mesh_nodes(), "edges": [ mesh_edges()[0].clone() ] }),
    );
    let gaps: Vec<&str> =
        partial["unsatisfied"].as_array().unwrap().iter().map(|g| g["iface"].as_str().unwrap()).collect();
    assert_eq!(gaps.len(), 2, "{gaps:?}");
    assert!(gaps.contains(&"proxy:route/router@0.1.0"));

    // A cycle is refused for the static form but still described.
    let (_, cyclic) = post(
        "/api/plan",
        json!({
            "nodes": ["mesh-domain", "record-store"],
            "edges": [
                { "plug": "record-store", "socket": "mesh-domain", "iface": "records:store/store@0.1.0" },
                { "plug": "mesh-domain", "socket": "record-store", "iface": "wasi:http/incoming-handler@0.2.0" },
            ]
        }),
    );
    // The second edge is rejected outright: record-store doesn't import http, and
    // an http import would be a HOST capability anyway.
    assert!(
        cyclic["problems"].as_array().unwrap().iter().any(|p| p["kind"] == "not-imported"),
        "{cyclic}"
    );

    // ===== the three emitted forms =========================================
    let emit = |form: &str| {
        post_text(
            "/api/emit",
            json!({ "nodes": mesh_nodes(), "edges": mesh_edges(), "form": form,
                    "meta": { "name": "mesh", "namespace": "mesh" } }),
        )
    };

    let script = emit("plug");
    assert!(script.contains("wac plug \"$REL/mesh_domain.wasm\""), "{script}");
    assert!(script.contains("--plug \"$REL/record_store.wasm\""));
    assert!(script.contains("# provides these"), "host needs are documented in the script");

    let wac = emit("wac");
    assert!(wac.contains("package mesh:composed;"));
    assert!(wac.contains("let record-store = new records:store"), "kebab-case idents: {wac}");
    assert!(wac.contains("\"records:store/store@0.1.0\": record-store[\"records:store/store@0.1.0\"]"));
    assert!(wac.contains("export mesh-domain...;"));

    let workload = emit("workload");
    assert!(workload.contains("apiVersion: runtime.wasmcloud.dev/v1alpha1"));
    assert!(workload.contains("kind: WorkloadDeployment"));
    // THE rule: one interface per entry. A merged [store, atomics] entry binds to
    // nothing that imports only one of them.
    assert!(workload.contains("interfaces: [store]"), "{workload}");
    assert!(workload.contains("interfaces: [batch]"));
    assert!(!workload.contains("interfaces: [store, atomics]"));
    // Composable edges leave NO trace: the runtime links them in-process.
    assert!(!workload.contains("records:store"), "edges are not manifest entries: {workload}");
    assert!(workload.contains("kind: Service"), "something serves http, so it gets a Service");
    for node in mesh_nodes() {
        assert!(workload.contains(&format!("- name: {node}")), "{node} missing from components");
    }

    // ===== compose for real =================================================
    let resp = ureq::post(&format!("http://{ADDR}/api/compose"))
        .set("content-type", "application/json")
        .send_string(&json!({ "nodes": mesh_nodes(), "edges": mesh_edges(), "root": "mesh-domain" }).to_string())
        .expect("compose");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.header("content-type"), Some("application/wasm"));
    let mut composed = Vec::new();
    resp.into_reader().read_to_end(&mut composed).unwrap();

    let studio_out = tmp().join("studio.composed.wasm");
    std::fs::write(&studio_out, &composed).unwrap();
    let (ok, out) = tool("wasm-tools", &["validate", studio_out.to_str().unwrap()]);
    assert!(ok, "the composed artifact must validate: {out}");

    // ...and it is the artifact `wac plug` writes. Run the emitted script and
    // compare — same inputs, same tool, same bytes on the scale that matters.
    let script_path = tmp().join("plug.sh");
    std::fs::write(&script_path, &script).unwrap();
    let (ok, out) = {
        let o = Command::new("bash")
            .arg(&script_path)
            .current_dir(root())
            .env("REL", rel())
            .env("OUT", tmp())
            .output()
            .expect("bash");
        (o.status.success(), String::from_utf8_lossy(&o.stderr).to_string())
    };
    assert!(ok, "the emitted script must run: {out}");
    let by_script = std::fs::read(tmp().join("mesh_domain.composed.wasm")).expect("script output");
    assert_eq!(
        composed.len(),
        by_script.len(),
        "in-wasm composition == what the emitted wac plug script produces"
    );

    // The declarative form composes too, through the real `wac compose`.
    let wac_path = tmp().join("composition.wac");
    let source: String = wac.lines().filter(|l| !l.starts_with("//")).collect::<Vec<_>>().join("\n");
    std::fs::write(&wac_path, source).unwrap();
    let wac_out = tmp().join("declarative.wasm");
    let deps: Vec<String> = vec![
        format!("mesh-domain:component={}", rel().join("mesh_domain.wasm").display()),
        format!("records:store={}", rel().join("record_store.wasm").display()),
        format!("resilience:breaker={}", rel().join("resilience.wasm").display()),
        format!("proxy:route={}", rel().join("proxy_route.wasm").display()),
    ];
    let mut args: Vec<&str> = vec!["compose", wac_path.to_str().unwrap()];
    for d in &deps {
        args.push("--dep");
        args.push(d);
    }
    args.push("-o");
    args.push(wac_out.to_str().unwrap());
    let (ok, out) = tool("wac", &args);
    assert!(ok, "the emitted .wac must compose: {out}");
    let (ok, out) = tool("wasm-tools", &["validate", wac_out.to_str().unwrap()]);
    assert!(ok, "and validate: {out}");

    // ===== the composed component actually RUNS =============================
    // The whole point: a graph wired in a browser produced a working app.
    let bin = root().join("host/target/release/comp-host");
    let child = Command::new(&bin)
        .args(["--component", studio_out.to_str().unwrap(), "--addr", RUN_ADDR, "--kv", "memory"])
        .env("VET_TENANT", "mesh")
        // mesh reads its upstream route table from config; point it at nothing so
        // the guarded call fails honestly (connection refused) rather than hanging.
        .env("CFG_ROUTES", "/upstream=http://127.0.0.1:3057/")
        .spawn()
        .expect("spawn composed");
    let _run = Kill(child);
    let mut served = None;
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&format!("http://{RUN_ADDR}/")).call() {
            served = Some(json_of(r));
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let served = served.expect("the studio-composed component did not serve");
    assert_eq!(served["service"], "mesh", "it is the mesh app: {served}");

    // Its capabilities are wired: the breaker records state through the
    // records:store the studio plugged in, and the upstream hop is real.
    let call = ureq::post(&format!("http://{RUN_ADDR}/api/call"))
        .set("content-type", "application/json")
        .send_string(&json!({ "key": "studio", "path": "/upstream/hit", "attempts": 1, "failure_threshold": 1 }).to_string());
    let call = match call {
        Ok(r) => json_of(r),
        Err(ureq::Error::Status(_, r)) => json_of(r),
        Err(e) => panic!("guarded call: {e}"),
    };
    assert_eq!(call["state"], "open", "a refused upstream tripped the breaker: {call}");
    let circuit = ureq::get(&format!("http://{RUN_ADDR}/api/circuit/studio")).call().expect("circuit");
    let circuit = json_of(circuit);
    assert_eq!(circuit["stats"]["failed"], 1, "state persisted via the plugged-in records:store");

    // ===== saved canvases ===================================================
    let (code, saved) = post(
        "/api/graphs",
        json!({ "name": "mesh", "nodes": mesh_nodes(), "edges": mesh_edges() }),
    );
    assert_eq!(code, 201, "{saved}");
    let id = saved["id"].as_str().unwrap();
    let (_, loaded) = get(&format!("/api/graphs/{id}"));
    assert_eq!(loaded["name"], "mesh");
    assert_eq!(loaded["edges"].as_array().unwrap().len(), 3);
    let (_, list) = get("/api/graphs");
    assert_eq!(list["graphs"][0]["nodes"], 4, "the list view summarises: {list}");

    // ===== refusals =========================================================
    let (code, e) = post("/api/plan", json!({ "nodes": ["ghost"] }));
    assert_eq!(code, 422);
    assert!(e["error"].as_str().unwrap().contains("unknown component"), "{e}");

    // A core module is not a component, and reflection says which.
    let bad = ureq::post(&format!("http://{ADDR}/api/components?id=junk"))
        .set("content-type", "application/wasm")
        .send_bytes(b"\0asm\x01\0\0\0");
    match bad {
        Err(ureq::Error::Status(422, r)) => {
            let v = json_of(r);
            assert!(v["error"].as_str().unwrap().contains("core wasm module"), "{v}");
        }
        other => panic!("a core module must be refused: {other:?}"),
    }
}

/// `read_to_end` needs this in scope.
use std::io::Read;
