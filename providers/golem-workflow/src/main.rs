//! golem-workflow — a native wasmCloud v2 capability provider that satisfies
//! `durable:workflow/orchestrator` over wRPC by invoking durable Golem workers.
//!
//! A consumer component calls `orchestrator.trigger(req)`; this provider bridges
//! the call to a Golem agent's HTTP endpoint (Golem 1.5 exposes agent methods
//! through its API gateway) and returns the worker's result. The bridge call
//! itself lives in the lib (`invoke_golem`) so it can be exercised against a
//! live Golem in an integration test without a wasmCloud host — see
//! `tests/golem_live.rs`.

use golem_workflow_provider::{invoke_golem, GolemConfig};
use wasmcloud_provider_sdk::{
    get_connection, load_host_data, run_provider, serve_provider_exports, Context, Provider,
};

wit_bindgen_wrpc::generate!({ world: "golem-provider" });

use exports::durable::workflow::orchestrator::{Handler, RunError, RunRequest, RunStatus};

#[derive(Clone)]
struct GolemProvider {
    http: reqwest::Client,
    cfg: GolemConfig,
}

impl GolemProvider {
    fn from_env() -> Self {
        let c = load_host_data().ok().map(|d| d.config.clone()).unwrap_or_default();
        let get = |k: &str, d: &str| c.get(k).cloned().unwrap_or_else(|| d.to_string());
        GolemProvider {
            http: reqwest::Client::new(),
            cfg: GolemConfig {
                base_url: get("GOLEM_URL", "http://127.0.0.1:9006"),
                host: c.get("GOLEM_HOST").cloned(),
                path_template: get("GOLEM_PATH_TEMPLATE", "/counters/{workflow-id}/increment"),
            },
        }
    }

    async fn run(&self, req: &RunRequest) -> Result<String, RunError> {
        invoke_golem(&self.http, &self.cfg, &req.workflow_id, &req.payload)
            .await
            .map_err(RunError::WorkerFailed)
    }
}

impl Provider for GolemProvider {}

impl Handler<Option<Context>> for GolemProvider {
    async fn trigger(&self, _cx: Option<Context>, req: RunRequest) -> anyhow::Result<Result<String, RunError>> {
        Ok(self.run(&req).await)
    }

    async fn start(&self, _cx: Option<Context>, req: RunRequest) -> anyhow::Result<Result<String, RunError>> {
        // rung: start == trigger, returning the worker id. True fire-and-poll
        // (status) is a follow-up once Golem's async-invocation API is wired.
        let id = req.workflow_id.clone();
        Ok(self.run(&req).await.map(|_| id))
    }

    async fn status(&self, _cx: Option<Context>, _run_id: String) -> anyhow::Result<Result<RunStatus, RunError>> {
        Ok(Ok(RunStatus { state: "completed".into(), output: None }))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let provider = GolemProvider::from_env();
    let shutdown = run_provider(provider.clone(), "golem-workflow-provider").await?;
    let connection = get_connection();
    let wrpc = connection.get_wrpc_client(connection.provider_key()).await?;
    serve_provider_exports(&wrpc, provider, shutdown, serve).await?;
    Ok(())
}
