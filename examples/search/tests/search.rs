//! E2E for the search console (SEARCH.md) as ONE composed wasm HTTP component
//! on the native Rust host. The read/query axis: seed a corpus, then prove
//! ranked retrieval, all-mode intersection, tag-facet filtering, and that a
//! repeat query is served from cache (hit-ratio rises).

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::Value;

const ADDR: &str = "127.0.0.1:3029";

struct HostGuard(Child);
impl Drop for HostGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn base() -> String {
    format!("http://{ADDR}")
}

fn req(method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let url = format!("{}{}", base(), path);
    let r = ureq::request(method, &url);
    let result = match &body {
        Some(b) => r.set("content-type", "application/json").send_string(&b.to_string()),
        None => r.call(),
    };
    let resp = match result {
        Ok(resp) => resp,
        Err(ureq::Error::Status(_, resp)) => resp,
        Err(e) => panic!("{method} {path}: {e}"),
    };
    let status = resp.status();
    (status, serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null))
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/vet-host");
    let component = root.join("components/target/search_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-search`)");
    assert!(component.exists(), "composed wasm missing (just compose-search)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "search")
        .spawn()
        .expect("spawn vet-host");
    let guard = HostGuard(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&base()).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("search host did not start");
}

fn titles(hits: &Value) -> Vec<String> {
    hits["hits"].as_array().unwrap().iter().map(|h| h["title"].as_str().unwrap_or("").to_string()).collect()
}

#[test]
fn seed_rank_facet_and_cache() {
    let _host = start_host();

    // seed the demo corpus.
    let (status, s) = req("POST", "/api/seed", None);
    assert_eq!(status, 200, "seed: {s}");
    assert!(s["seeded"].as_u64().unwrap() >= 10);

    // ranked retrieval: a rare term ("saga") ranks its doc first.
    let (status, r) = req("GET", "/api/search?q=saga&mode=any&limit=5", None);
    assert_eq!(status, 200, "search: {r}");
    let t = titles(&r);
    assert!(!t.is_empty(), "saga should match something");
    assert!(t[0].to_lowercase().contains("saga"), "top hit for 'saga' should be the saga doc, got {t:?}");

    // all-mode intersection shrinks the set vs any-mode.
    let (_, any) = req("GET", "/api/search?q=distributed+index&mode=any&limit=20", None);
    let (_, all) = req("GET", "/api/search?q=distributed+index&mode=all&limit=20", None);
    let any_n = any["total"].as_u64().unwrap();
    let all_n = all["total"].as_u64().unwrap();
    assert!(all_n <= any_n, "all-mode ({all_n}) must not exceed any-mode ({any_n})");

    // tag facet restricts hits: only kind:note docs.
    let (_, noted) = req("GET", "/api/search?q=key&mode=any&tags=kind:note&limit=20", None);
    for h in noted["hits"].as_array().unwrap() {
        let tags = h["tags"].as_array().unwrap();
        assert!(tags.iter().any(|t| t == "kind:note"), "faceted hit must carry kind:note: {h}");
    }

    // cache: the SAME query twice — second is a cache hit.
    let (_, first) = req("GET", "/api/search?q=encryption&mode=any&limit=5", None);
    assert_eq!(first["cached"], false, "first query is a miss");
    let (_, second) = req("GET", "/api/search?q=encryption&mode=any&limit=5", None);
    assert_eq!(second["cached"], true, "identical repeat query should be served from cache");

    // stats reflect at least one hit + some misses, and the seeded doc count.
    let (_, st) = req("GET", "/api/stats", None);
    assert!(st["docs"].as_u64().unwrap() >= 10, "doc count: {st}");
    assert!(st["cache_hits"].as_u64().unwrap() >= 1, "at least one cache hit: {st}");
    assert!(st["hit_ratio"].as_f64().unwrap() > 0.0, "hit-ratio should be positive: {st}");
}
