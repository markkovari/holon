//! Free of the lattice, provider SDK and wasmtime so it is unit-testable; `src/main.rs` wires it.

use golem_wasm_rpc::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct RunRequest {
    pub workflow_id: String,
    pub payload: String,
}

/// `trigger-workflow` takes one record whose fields are positional, so order matters.
pub fn to_worker_args(req: &RunRequest) -> Vec<Value> {
    vec![Value::Record(vec![
        Value::String(req.workflow_id.clone()),
        Value::String(req.payload.clone()),
    ])]
}

/// Also accepts a bare `Value::String`: some invoke paths unwrap a single non-result return.
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


/// Golem 1.5 agents are reached over the API gateway's HTTP endpoints, not the typed worker API.
#[derive(Clone, Debug)]
pub struct GolemConfig {
    pub base_url: String,
    /// `Host` header for gateway subdomain routing, e.g. `bookapp.localhost:9006`.
    pub host: Option<String>,
    pub path_template: String,
}

/// In the lib, not the binary, so a test can run it against a live Golem.
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

/// Gated on `GOLEM_E2E=1`: needs a running Golem with the `book:flight` agent (see `e2e.sh`).
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
        // Same worker twice: the durable count must advance, which a stub cannot fake.
        let a = invoke_golem(&http, &cfg, "e2e-agent", "").await.expect("first invoke");
        let b = invoke_golem(&http, &cfg, "e2e-agent", "").await.expect("second invoke");
        let na: i64 = a.trim().parse().unwrap_or_else(|_| panic!("non-numeric result: {a:?}"));
        let nb: i64 = b.trim().parse().unwrap_or_else(|_| panic!("non-numeric result: {b:?}"));
        assert_eq!(nb, na + 1, "durable worker state must advance: {na} -> {nb}");
    }
}
