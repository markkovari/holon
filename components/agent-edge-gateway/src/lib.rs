//! `agent-edge-gateway` — serverless agent coordination and CAS storage gateway for Cloudflare Workers & R2

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "wit",
        ],
        world: "holon:agent-edge-gateway/gateway",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

pub mod cas;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};
use cas::{CasStore, CheckpointRequest};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::OnceLock;

const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

fn global_store() -> &'static CasStore {
    static STORE: OnceLock<CasStore> = OnceLock::new();
    STORE.get_or_init(CasStore::new)
}

struct Component;

pub struct HttpResult {
    pub status: u16,
    pub content_type: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResult {
    pub fn json(status: u16, val: Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            headers: Vec::new(),
            body: val.to_string().into_bytes(),
        }
    }

    pub fn with_etag(mut self, etag: &str) -> Self {
        self.headers.push(("ETag".to_string(), etag.to_string()));
        self
    }

    pub fn octet_stream(status: u16, body: Vec<u8>) -> Self {
        Self { status, content_type: "application/octet-stream", headers: Vec::new(), body }
    }
}

pub fn handle_request(
    store: &CasStore,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
    timestamp_ms: u64,
) -> HttpResult {
    let clean_path = path.split('?').next().unwrap_or("/").trim_matches('/');
    let seg: Vec<&str> =
        if clean_path.is_empty() { Vec::new() } else { clean_path.split('/').collect() };

    let if_match =
        headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("if-match")).map(|(_, v)| v.as_str());

    match (method, seg.as_slice()) {
        ("GET", ["health"]) | ("GET", []) => HttpResult::json(
            200,
            json!({
                "service": "holon:agent-edge-gateway",
                "version": "0.1.0",
                "status": "ok"
            }),
        ),

        // POST /v1/cas/objects -> store content-addressed object
        ("POST", ["v1", "cas", "objects"]) => {
            let oid = store.put_object(body);
            HttpResult::json(200, json!({ "oid": oid, "size": body.len() }))
        }

        // GET /v1/cas/objects/{oid} -> retrieve object
        ("GET", ["v1", "cas", "objects", oid]) => match store.get_object(oid) {
            Some(data) => HttpResult::octet_stream(200, data),
            None => HttpResult::json(404, json!({ "error": "object not found", "oid": oid })),
        },

        // GET /v1/manifests/{repo}/{branch} -> get manifest + ETag
        ("GET", ["v1", "manifests", repo, branch]) => match store.get_manifest(repo, branch) {
            Some((manifest, etag)) => {
                HttpResult::json(200, serde_json::to_value(&manifest).unwrap()).with_etag(&etag)
            }
            None => HttpResult::json(
                404,
                json!({ "error": "manifest not found", "repo": repo, "branch": branch }),
            ),
        },

        // PUT /v1/manifests/{repo}/{branch} -> atomic CAS update
        ("PUT", ["v1", "manifests", repo, branch]) => {
            #[derive(Deserialize)]
            struct PutManifestReq {
                head_commit_oid: String,
                agent_id: String,
            }
            let req: PutManifestReq = match serde_json::from_slice(body) {
                Ok(r) => r,
                Err(e) => {
                    return HttpResult::json(
                        400,
                        json!({ "error": "bad request", "details": e.to_string() }),
                    )
                }
            };

            match store.put_manifest_cas(
                repo,
                branch,
                &req.head_commit_oid,
                &req.agent_id,
                if_match,
                timestamp_ms,
            ) {
                Ok((manifest, etag)) => {
                    HttpResult::json(200, serde_json::to_value(&manifest).unwrap()).with_etag(&etag)
                }
                Err(conflict) => HttpResult::json(412, serde_json::to_value(&conflict).unwrap()),
            }
        }

        // GET /v1/commits/{oid} -> read commit
        ("GET", ["v1", "commits", oid]) => match store.get_commit(oid) {
            Some(commit) => HttpResult::json(200, serde_json::to_value(&commit).unwrap()),
            None => HttpResult::json(404, json!({ "error": "commit not found", "oid": oid })),
        },

        // POST /v1/checkpoints/{repo} -> atomic agent checkpoint
        ("POST", ["v1", "checkpoints", repo]) => {
            let req: CheckpointRequest = match serde_json::from_slice(body) {
                Ok(r) => r,
                Err(e) => {
                    return HttpResult::json(
                        400,
                        json!({ "error": "bad request", "details": e.to_string() }),
                    )
                }
            };

            match store.commit_checkpoint(repo, &req, timestamp_ms) {
                Ok(success) => HttpResult::json(200, serde_json::to_value(&success).unwrap())
                    .with_etag(&success.etag),
                Err(conflict) => HttpResult::json(412, serde_json::to_value(&conflict).unwrap()),
            }
        }

        _ => HttpResult::json(404, json!({ "error": "not found", "path": path })),
    }
}

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = match request.get_method() {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Head => "HEAD",
            _ => "OTHER",
        };
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let body = read_body(request).await.unwrap_or_default();

        let store = global_store();
        let timestamp_ms = 1_700_000_000_000; // In WASI guest this would read wall-clock
        let res = handle_request(store, method, &path, &[], &body, timestamp_ms);

        let headers = crate::bindings::p3::http::types::Fields::new();
        let _ = headers.set("content-type", &[res.content_type.as_bytes().to_vec()]);
        for (k, v) in res.headers {
            let _ = headers.append(&k, v.as_bytes());
        }

        respond_with(res.status, headers, res.body)
    }
}

bindings::export!(Component with_types_in bindings);

guestio::guest_p3_respond!();
guestio::guest_p3_read_body!(MAX_BODY_BYTES);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cas_object_put_and_get() {
        let store = CasStore::new();
        let payload = b"agent state snapshot memory blob";
        let res = handle_request(&store, "POST", "/v1/cas/objects", &[], payload, 1000);
        assert_eq!(res.status, 200);

        let v: Value = serde_json::from_slice(&res.body).unwrap();
        let oid = v["oid"].as_str().unwrap();
        assert!(oid.starts_with("sha256:"));

        let get_res =
            handle_request(&store, "GET", &format!("/v1/cas/objects/{oid}"), &[], &[], 1000);
        assert_eq!(get_res.status, 200);
        assert_eq!(get_res.body, payload);
    }

    #[test]
    fn test_cas_manifest_creation_and_update() {
        let store = CasStore::new();

        // 1. Initial creation (no If-Match)
        let body = json!({
            "head_commit_oid": "sha256:commit0",
            "agent_id": "agent-1"
        })
        .to_string();

        let res =
            handle_request(&store, "PUT", "/v1/manifests/my-repo/main", &[], body.as_bytes(), 1000);
        assert_eq!(res.status, 200);
        let etag1 = res.headers.iter().find(|(k, _)| k == "ETag").unwrap().1.clone();

        // 2. Fetch manifest
        let get_res = handle_request(&store, "GET", "/v1/manifests/my-repo/main", &[], &[], 1000);
        assert_eq!(get_res.status, 200);
        let m: Value = serde_json::from_slice(&get_res.body).unwrap();
        assert_eq!(m["generation"], 1);
        assert_eq!(m["head_commit_oid"], "sha256:commit0");

        // 3. Update with valid ETag
        let body2 = json!({
            "head_commit_oid": "sha256:commit1",
            "agent_id": "agent-2"
        })
        .to_string();

        let res2 = handle_request(
            &store,
            "PUT",
            "/v1/manifests/my-repo/main",
            &[("If-Match".to_string(), etag1.clone())],
            body2.as_bytes(),
            2000,
        );
        assert_eq!(res2.status, 200);
        let m2: Value = serde_json::from_slice(&res2.body).unwrap();
        assert_eq!(m2["generation"], 2);
        assert_eq!(m2["head_commit_oid"], "sha256:commit1");

        // 4. Stale ETag fails with 412 Precondition Failed
        let res_stale = handle_request(
            &store,
            "PUT",
            "/v1/manifests/my-repo/main",
            &[("If-Match".to_string(), etag1)],
            body2.as_bytes(),
            3000,
        );
        assert_eq!(res_stale.status, 412);
        let conflict: Value = serde_json::from_slice(&res_stale.body).unwrap();
        assert_eq!(conflict["current_head"], "sha256:commit1");
    }

    #[test]
    fn test_agent_checkpoint_commit_flow() {
        let store = CasStore::new();

        // Agent 1 commits first checkpoint on branch
        let req1 = json!({
            "branch": "feature/agent-exp",
            "agent_id": "agent-alpha",
            "state_root": "sha256:state_root_1",
            "capability_log_head": "sha256:log_head_1",
            "message": "Initial agent exploration step",
            "expected_etag": null
        })
        .to_string();

        let res1 =
            handle_request(&store, "POST", "/v1/checkpoints/repo-x", &[], req1.as_bytes(), 1000);
        assert_eq!(res1.status, 200);
        let val1: Value = serde_json::from_slice(&res1.body).unwrap();
        let commit1_oid = val1["commit"]["oid"].as_str().unwrap();
        let etag1 = val1["etag"].as_str().unwrap().to_string();
        assert_eq!(val1["manifest"]["generation"], 1);

        // Agent 2 commits next step pointing to parent
        let req2 = json!({
            "branch": "feature/agent-exp",
            "agent_id": "agent-alpha",
            "state_root": "sha256:state_root_2",
            "capability_log_head": "sha256:log_head_2",
            "message": "Step 2 after tool invocation",
            "expected_etag": etag1
        })
        .to_string();

        let res2 =
            handle_request(&store, "POST", "/v1/checkpoints/repo-x", &[], req2.as_bytes(), 2000);
        assert_eq!(res2.status, 200);
        let val2: Value = serde_json::from_slice(&res2.body).unwrap();
        assert_eq!(val2["manifest"]["generation"], 2);
        assert_eq!(val2["commit"]["parent_oids"][0], commit1_oid);
    }
}
