//! `inproc-workflow` — the default execution backend for the jobs queue.
//!
//! Implements `durable:workflow/orchestrator` in-process. `trigger` decides a
//! job's outcome from its workflow id + JSON payload; the queue owns durability
//! (retry/backoff/DLQ over the outbox), so this stays a pure decision function.
//! A few demo workflows:
//!   - `email` / `resize` / `report` / `echo` — succeed.
//!   - `flaky`  — fails while `attempt < fail_until` (the queue passes `attempt`),
//!                then succeeds; exercises retry + backoff.
//!   - `boom`   — always fails; exercises the dead-letter path.
//! Anything else is `not-found`. `start`/`status` are the Golem backend's job.

#[allow(warnings)]
mod bindings;

use bindings::exports::durable::workflow::orchestrator::{
    Guest, RunError, RunRequest, RunStatus,
};
use serde_json::Value;

struct Component;

impl Guest for Component {
    fn trigger(req: RunRequest) -> Result<String, RunError> {
        let payload: Value = if req.payload.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&req.payload)
                .map_err(|e| RunError::InvalidInput(format!("payload not JSON: {e}")))?
        };
        match req.workflow_id.as_str() {
            "email" => Ok(r#"{"sent":true}"#.into()),
            "resize" => Ok(r#"{"resized":true}"#.into()),
            "report" => Ok(r#"{"rows":42}"#.into()),
            "echo" => Ok(req.payload.clone()),
            "flaky" => {
                let attempt = payload["attempt"].as_u64().unwrap_or(1);
                let fail_until = payload["fail_until"].as_u64().unwrap_or(3);
                if attempt < fail_until {
                    Err(RunError::WorkerFailed(format!(
                        "transient failure (attempt {attempt} < {fail_until})"
                    )))
                } else {
                    Ok(format!(r#"{{"ok_after":{attempt}}}"#))
                }
            }
            "boom" => Err(RunError::WorkerFailed("permanent failure".into())),
            other => Err(RunError::NotFound(other.into())),
        }
    }

    fn start(_req: RunRequest) -> Result<String, RunError> {
        Err(RunError::Unavailable(
            "in-process backend is synchronous; use trigger (async start/status is the Golem backend)".into(),
        ))
    }

    fn status(_run_id: String) -> Result<RunStatus, RunError> {
        Err(RunError::Unavailable("in-process backend has no async runs".into()))
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;
    fn run(id: &str, payload: &str) -> Result<String, RunError> {
        <Component as Guest>::trigger(RunRequest {
            workflow_id: id.into(),
            payload: payload.into(),
        })
    }

    #[test]
    fn success_and_notfound() {
        assert!(run("email", "{}").is_ok());
        assert!(matches!(run("nope", "{}"), Err(RunError::NotFound(_))));
    }

    #[test]
    fn flaky_fails_then_succeeds() {
        assert!(matches!(run("flaky", r#"{"attempt":1,"fail_until":3}"#), Err(RunError::WorkerFailed(_))));
        assert!(matches!(run("flaky", r#"{"attempt":2,"fail_until":3}"#), Err(RunError::WorkerFailed(_))));
        assert!(run("flaky", r#"{"attempt":3,"fail_until":3}"#).is_ok());
    }

    #[test]
    fn boom_always_fails() {
        assert!(matches!(run("boom", "{}"), Err(RunError::WorkerFailed(_))));
    }
}
