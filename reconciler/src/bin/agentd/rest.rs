//! `api/openapi.yaml` over axum: JSON in and out, SSE for the event stream,
//! RFC 9457 problems for refusals.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::service::{Daemon, Fail};
use super::wire;

type Shared = Arc<Daemon>;

fn problem(status: StatusCode, kind: &str, detail: String) -> Response {
    let body = wire::Problem {
        kind: format!("https://holon.dev/errors/{kind}"),
        title: kind.replace('-', " "),
        status: status.as_u16(),
        detail,
    };
    let mut r = (status, Json(body)).into_response();
    r.headers_mut().insert("content-type", "application/problem+json".parse().unwrap());
    r
}

fn fail(f: Fail) -> Response {
    match f {
        Fail::NotFound(d) => problem(StatusCode::NOT_FOUND, "not-found", d),
        Fail::Invalid(kind, d) => problem(StatusCode::BAD_REQUEST, kind, d),
        Fail::Conflict(kind, d) => problem(StatusCode::CONFLICT, kind, d),
        Fail::Internal(d) => problem(StatusCode::INTERNAL_SERVER_ERROR, "internal", d),
    }
}

fn reply<T: Serialize>(ok: StatusCode, r: Result<T, Fail>) -> Response {
    match r {
        Ok(v) => (ok, Json(v)).into_response(),
        Err(f) => fail(f),
    }
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, Fail> {
    serde_json::from_slice(body).map_err(|e| Fail::Invalid("bad-request", e.to_string()))
}

async fn create(State(d): State<Shared>, body: axum::body::Bytes) -> Response {
    reply(StatusCode::CREATED, parse(&body).and_then(|r| d.create_session(r)))
}

async fn show(State(d): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    reply(StatusCode::OK, d.get_session(&id))
}

async fn close(State(d): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    reply(StatusCode::OK, d.close_session(&id))
}

async fn send_task(
    State(d): State<Shared>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let key = headers.get("idempotency-key").and_then(|v| v.to_str().ok()).map(String::from);
    let r = parse::<wire::SendTask>(&body).and_then(|t| d.send_task(&id, t.prompt, key));
    reply(StatusCode::ACCEPTED, r)
}

async fn cancel(
    State(d): State<Shared>,
    UrlPath((id, task)): UrlPath<(String, String)>,
) -> Response {
    reply(StatusCode::OK, d.cancel_task(&id, &task))
}

async fn approve(
    State(d): State<Shared>,
    UrlPath((id, call_id)): UrlPath<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    reply(StatusCode::OK, parse(&body).and_then(|r| d.submit_approval(&id, &call_id, r)))
}

#[derive(Deserialize)]
struct After {
    after_seq: Option<u64>,
}

async fn events(
    State(d): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<After>,
    headers: HeaderMap,
) -> Response {
    let last_id =
        headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    let stream = match d.events(&id, last_id.or(q.after_seq).unwrap_or(0)) {
        Ok(s) => s,
        Err(f) => return fail(f),
    };
    let frames = stream.map(|e| {
        Ok::<_, Infallible>(
            SseEvent::default()
                .id(e.seq.to_string())
                .event(e.payload.kind())
                .data(serde_json::to_string(&e).unwrap_or_default()),
        )
    });
    Sse::new(frames)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("keepalive"))
        .into_response()
}

async fn health(State(d): State<Shared>) -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true, "sessions": d.session_count()}))
}

/// The authenticated routes; `/health` is added open by the caller.
pub fn routes(d: Shared) -> Router {
    Router::new()
        .route("/v1/sessions", post(create))
        .route("/v1/sessions/{id}", get(show).delete(close))
        .route("/v1/sessions/{id}/tasks", post(send_task))
        .route("/v1/sessions/{id}/tasks/{task_id}/cancel", post(cancel))
        .route("/v1/sessions/{id}/events", get(events))
        .route("/v1/sessions/{id}/tool-calls/{call_id}/approval", post(approve))
        .with_state(d)
}

pub fn health_route(d: Shared) -> Router {
    Router::new().route("/health", get(health)).with_state(d)
}
