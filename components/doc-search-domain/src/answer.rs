//! `answer` — the only part that spends anything.
//!
//! Order is the specification: step-up, then cache, then retrieval, then budget,
//! then the model. Each stops the cost of the next.

use crate::bindings::ai::inference::inference as ai;
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types as auth_types;
use crate::bindings::cache::store::cache;
use crate::bindings::quota::meter::meter;
use crate::bindings::records::store::store as records;
use crate::bindings::search::index::index as search;
use crate::bindings::wasi::http::types::Method;
use crate::{cfg_u64, now_secs, Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    if !matches!(method, Method::Post) {
        return Reply::err(404, "not_found");
    }

    if route.bearer.is_empty() {
        return Reply::err(401, "unauthenticated");
    }

    let principal = match authz::authorize(
        &route.bearer,
        &auth_types::Permission { target: "docs".into(), action: "read".into() },
    ) {
        Ok(p) => p,
        Err(auth_types::AuthError::InvalidToken(_))
        | Err(auth_types::AuthError::Expired)
        | Err(auth_types::AuthError::Malformed(_)) => {
            return Reply::err(401, "unauthenticated")
        }
        Err(auth_types::AuthError::InsufficientScope(_)) => return Reply::err(403, "forbidden"),
        Err(auth_types::AuthError::BackendUnavailable(_))
        | Err(auth_types::AuthError::Internal(_)) => return Reply::err(503, "auth_unavailable"),
        Err(_) => return Reply::err(401, "unauthenticated"),
    };
    let subject = principal.subject.as_str();

    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let question = req.get("question").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if question.is_empty() {
        return Reply::err(400, "invalid_question");
    }

    // 1. Step-up: an index lookup keyed by subject; find_by wants the value
    // JSON-encoded, and a wrong query silently reads as "never verified".
    let subject_json = serde_json::to_string(subject).unwrap_or_default();
    let stepup_entries = match records::find_by("stepups", "subject", &subject_json) {
        Ok(entries) => entries,
        Err(_) => return Reply::err(503, "answer_unavailable"),
    };
    let latest_verified_at = stepup_entries
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .filter_map(|v| v.get("verified_at").and_then(Value::as_u64))
        .max()
        .unwrap_or(0);
    let stepup_ttl = cfg_u64("stepup-ttl-secs", 900);
    let now = now_secs();
    if now.saturating_sub(latest_verified_at) > stepup_ttl {
        return Reply::err(403, "step_up_required");
    }

    // 2. Cache — a hit costs nothing: no quota spent, no model call.
    let cache_key = format!("answer:{question}");
    let budget_limit = cfg_u64("answer-budget", 50);
    let period_secs = cfg_u64("answer-period-secs", 86400);
    if let Ok(Some(bytes)) = cache::get(&cache_key) {
        if let Ok(cached) = serde_json::from_slice::<Value>(&bytes) {
            let remaining = match meter::peek(subject, budget_limit, period_secs) {
                Ok(balance) => balance.remaining,
                Err(_) => 0,
            };
            return Reply::json(
                200,
                json!({
                    "answer": cached.get("answer").cloned().unwrap_or(Value::Null),
                    "sources": cached.get("sources").cloned().unwrap_or(json!([])),
                    "cached": true,
                    "remaining": remaining,
                }),
            );
        }
    }

    // 3. Retrieval — no hits is a 404, and nothing has been spent yet.
    let hits = match search::query(&question, search::Mode::Any, &[], 3) {
        Ok(h) => h,
        Err(_) => return Reply::err(503, "answer_unavailable"),
    };
    if hits.is_empty() {
        return Reply::err(404, "no_sources");
    }

    // 4. Budget — only now, after retrieval proved the question is answerable.
    let balance = match meter::reserve(subject, 1, budget_limit, period_secs) {
        Ok(b) => b,
        Err(meter::QuotaError::Exceeded(_)) => {
            // The payload is units still available (always 0 here), never a
            // duration. The real wait comes from a fresh peek.
            let retry_after = match meter::peek(subject, budget_limit, period_secs) {
                Ok(b) => b.resets_at.saturating_sub(now_secs()),
                Err(_) => 0,
            };
            return Reply::json(429, json!({ "error": "budget_exhausted", "retry_after": retry_after }));
        }
        Err(_) => return Reply::err(503, "answer_unavailable"),
    };

    // 5. The model, grounded only in what retrieval found.
    let mut sources = Vec::with_capacity(hits.len());
    let mut docs = Vec::with_capacity(hits.len());
    for hit in &hits {
        sources.push(hit.id.clone());
        if let Ok(entry) = records::get("docs", &hit.id) {
            let doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
            let title = doc.get("title").and_then(Value::as_str).unwrap_or("");
            let text = doc.get("text").and_then(Value::as_str).unwrap_or("");
            docs.push(format!("{title}\n{text}"));
        }
    }
    let context = docs.join("\n\n");

    let answer = match ai::generate(&question, &context) {
        Ok(a) => a,
        Err(_) => return Reply::err(503, "answer_unavailable"),
    };

    let ttl = cfg_u64("answer-cache-ttl-secs", 3600);
    let cache_body = json!({ "answer": answer, "sources": sources });
    let _ = cache::set(&cache_key, cache_body.to_string().as_bytes(), ttl);

    Reply::json(
        200,
        json!({ "answer": answer, "sources": sources, "cached": false, "remaining": balance.remaining }),
    )
}