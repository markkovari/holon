//! golem-workflow — the type-mapping core of the wRPC→Golem provider.
//!
//! The provider's job at each call is a translation: the consumer's typed
//! `run-request` becomes the arguments of a Golem worker invocation
//! (`golem_wasm_rpc::Value`), and the worker's `result<string, string>` return
//! becomes back the contract's `result<string, run-error>`. That translation is
//! this module — the part with the real bugs — kept free of the lattice, the
//! provider SDK, and even wasmtime, so it is exhaustively unit-testable on its
//! own. The binary (src/main.rs, rung 2) wires it to wRPC + golem-client.

use golem_wasm_rpc::Value;

/// A workflow invocation (mirrors `durable:workflow/orchestrator.run-request`).
#[derive(Debug, Clone, PartialEq)]
pub struct RunRequest {
    pub workflow_id: String,
    pub payload: String,
}

/// Map a `run-request` to the argument list of the Golem worker function
/// `trigger-workflow(req: run-request)` — a single `record { workflow-id,
/// payload }` argument, i.e. a positional `Record` of two strings.
pub fn to_worker_args(req: &RunRequest) -> Vec<Value> {
    vec![Value::Record(vec![
        Value::String(req.workflow_id.clone()),
        Value::String(req.payload.clone()),
    ])]
}

/// Interpret the worker's return `Value` as the contract's `result<string, _>`.
///
/// Golem worker functions declared `-> result<string, string>` return a
/// `Value::Result`; we also tolerate a bare `Value::String` (some invoke paths
/// unwrap a single non-result return) so the mapping is forgiving of the worker
/// author's exact signature.
pub fn from_worker_result(v: &Value) -> Result<String, String> {
    match v {
        Value::Result(Ok(Some(inner))) => expect_string(inner),
        Value::Result(Ok(None)) => Ok(String::new()),
        Value::Result(Err(Some(inner))) => Err(expect_string(inner).unwrap_or_else(|e| e)),
        Value::Result(Err(None)) => Err("worker returned an empty error".into()),
        Value::String(s) => Ok(s.clone()),
        other => Err(format!("unexpected worker return: {other:?}")),
    }
}

fn expect_string(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        other => Err(format!("expected string, got {other:?}")),
    }
}

// ---- the Golem call ----------------------------------------------------------

/// How to reach a Golem worker/agent's invoke endpoint. Golem 1.5 agents expose
/// HTTP endpoints through the API gateway (e.g. `POST /counters/{name}/increment`
/// on host `bookapp.localhost:9006`), so the provider just POSTs there — no
/// worker-service typed-value dance needed for the endpoint-style agents.
#[derive(Clone, Debug)]
pub struct GolemConfig {
    /// Gateway base, e.g. `http://127.0.0.1:9006`.
    pub base_url: String,
    /// `Host` header for gateway subdomain routing, e.g. `bookapp.localhost:9006`.
    pub host: Option<String>,
    /// Path with `{workflow-id}` substituted, e.g. `/counters/{workflow-id}/increment`.
    pub path_template: String,
}

/// Invoke a Golem agent endpoint and return its result body as a string. This is
/// the provider's actual bridge call — kept here (not in the binary) so it can be
/// exercised against a live Golem in an integration test without the wasmCloud host.
pub async fn invoke_golem(
    http: &reqwest::Client,
    cfg: &GolemConfig,
    workflow_id: &str,
    payload: &str,
) -> Result<String, String> {
    let path = cfg.path_template.replace("{workflow-id}", workflow_id);
    let url = format!("{}{}", cfg.base_url.trim_end_matches('/'), path);
    let mut rb = http.post(&url);
    if let Some(h) = &cfg.host {
        rb = rb.header("host", h);
    }
    if !payload.is_empty() {
        rb = rb.header("content-type", "application/json").body(payload.to_string());
    }
    let resp = rb.send().await.map_err(|e| format!("golem unreachable: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status.is_success() {
        Ok(body.trim().to_string())
    } else {
        Err(format!("golem {status}: {body}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> RunRequest {
        RunRequest { workflow_id: "book-flight".into(), payload: r#"{"from":"SFO"}"#.into() }
    }

    #[test]
    fn args_are_a_record_of_two_strings() {
        let args = to_worker_args(&req());
        assert_eq!(args.len(), 1, "trigger-workflow takes a single record arg");
        match &args[0] {
            Value::Record(fields) => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0], Value::String("book-flight".into()));
                assert_eq!(fields[1], Value::String(r#"{"from":"SFO"}"#.into()));
            }
            other => panic!("expected Record, got {other:?}"),
        }
    }

    #[test]
    fn ok_string_result_unwraps() {
        let v = Value::Result(Ok(Some(Box::new(Value::String("FL-123".into())))));
        assert_eq!(from_worker_result(&v), Ok("FL-123".into()));
    }

    #[test]
    fn err_string_result_becomes_err() {
        let v = Value::Result(Err(Some(Box::new(Value::String("no seats".into())))));
        assert_eq!(from_worker_result(&v), Err("no seats".into()));
    }

    #[test]
    fn empty_ok_and_err_are_handled() {
        assert_eq!(from_worker_result(&Value::Result(Ok(None))), Ok(String::new()));
        assert!(from_worker_result(&Value::Result(Err(None))).is_err());
    }

    #[test]
    fn bare_string_return_tolerated() {
        assert_eq!(from_worker_result(&Value::String("done".into())), Ok("done".into()));
    }

    #[test]
    fn wrong_type_is_an_error_not_a_panic() {
        assert!(from_worker_result(&Value::U64(7)).is_err());
        let wrapped_wrong = Value::Result(Ok(Some(Box::new(Value::U64(7)))));
        assert!(from_worker_result(&wrapped_wrong).is_err());
    }
}

/// Live end-to-end test of the actual bridge call against a RUNNING Golem.
/// Gated on `GOLEM_E2E=1` (needs `golem server run` + the `book:flight` agent
/// deployed at `bookapp.localhost:9006`) so a plain `cargo test` skips it.
#[cfg(test)]
mod live_e2e {
    use super::{invoke_golem, GolemConfig};

    #[tokio::test]
    async fn bridge_invokes_a_real_durable_golem_worker() {
        if std::env::var("GOLEM_E2E").is_err() {
            eprintln!("skipping live e2e — set GOLEM_E2E=1 with a running Golem");
            return;
        }
        let http = reqwest::Client::new();
        let cfg = GolemConfig {
            base_url: std::env::var("GOLEM_URL").unwrap_or_else(|_| "http://127.0.0.1:9006".into()),
            host: Some(std::env::var("GOLEM_HOST").unwrap_or_else(|_| "bookapp.localhost:9006".into())),
            path_template: "/counters/{workflow-id}/increment".into(),
        };
        // Trigger the SAME worker twice; the durable count must advance — proof
        // we invoked a real, stateful Golem worker through the provider's bridge,
        // not a stub.
        let a = invoke_golem(&http, &cfg, "e2e-agent", "").await.expect("first invoke");
        let b = invoke_golem(&http, &cfg, "e2e-agent", "").await.expect("second invoke");
        let na: i64 = a.trim().parse().unwrap_or_else(|_| panic!("non-numeric result: {a:?}"));
        let nb: i64 = b.trim().parse().unwrap_or_else(|_| panic!("non-numeric result: {b:?}"));
        assert_eq!(nb, na + 1, "durable worker state must advance: {na} -> {nb}");
    }
}
