use crate::bindings::ai::inference::inference as ai;
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
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

    let subject = match authorize_perm(route, "read") {
        Ok(s) => s,
        Err(r) => return r,
    };

    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let question = req.get("question").and_then(Value::as_str).unwrap_or("");
    if question.is_empty() {
        return Reply::err(400, "invalid_question");
    }

    // 1. Step-up
    let entries =
        records::find_by("stepups", "subject", &json!(subject).to_string()).unwrap_or_default();
    let is_stepped_up = entries.first().is_some_and(|entry| {
        let doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
        let verified_at = doc.get("verified_at").and_then(Value::as_u64).unwrap_or(0);
        let ttl = cfg_u64("stepup-ttl-secs", 900);
        verified_at > 0 && (now_secs() - verified_at) <= ttl
    });

    if !is_stepped_up {
        return Reply::err(403, "step_up_required");
    }

    // 2. Cache
    let cache_key = format!("answer:{}", question);
    if let Ok(Some(cached_bytes)) = cache::get(&cache_key) {
        if let Ok(cached_json) = serde_json::from_slice::<Value>(&cached_bytes) {
            let mut result = cached_json;
            result["cached"] = json!(true);

            // For cached hit, remaining is what meter::peek reports
            let budget = cfg_u64("answer-budget", 50);
            let period = cfg_u64("answer-period-secs", 86400);
            let remaining = match meter::peek(&subject, budget, period) {
                Ok(b) => b.remaining,
                Err(_) => 0,
            };
            result["remaining"] = json!(remaining);

            return Reply::json(200, result);
        }
    }

    // 3. Retrieval
    let hits = match search::query(question, search::Mode::Any, &[], 3) {
        Ok(h) => h,
        Err(_) => return Reply::err(500, "search_error"),
    };

    if hits.is_empty() {
        return Reply::err(404, "no_sources");
    }

    let mut context_parts = Vec::new();
    let mut source_ids = Vec::new();
    for hit in hits {
        source_ids.push(hit.id.clone());
        if let Ok(e) = records::get("docs", &hit.id) {
            let doc: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            let title = doc.get("title").and_then(Value::as_str).unwrap_or("");
            let text = doc.get("text").and_then(Value::as_str).unwrap_or("");
            context_parts.push(format!("{}\n{}", title, text));
        }
    }

    // 4. Budget
    let budget = cfg_u64("answer-budget", 50);
    let period = cfg_u64("answer-period-secs", 86400);

    match meter::reserve(&subject, 1, budget, period) {
        Ok(balance) => {
            // 5. The model
            let context = context_parts.join("\n\n");
            match ai::generate(question, &context) {
                Ok(answer_text) => {
                    let result = json!({
                        "answer": answer_text,
                        "sources": source_ids,
                        "cached": false,
                        "remaining": balance.remaining
                    });

                    let ttl = cfg_u64("answer-cache-ttl-secs", 3600);
                    let _ = cache::set(&cache_key, &serde_json::to_vec(&result).unwrap(), ttl);

                    Reply::json(200, result)
                }
                Err(_) => Reply::err(503, "answer_unavailable"),
            }
        }
        Err(crate::bindings::quota::meter::meter::QuotaError::Exceeded(_)) => {
            let resets_at = match meter::peek(&subject, budget, period) {
                Ok(b) => b.resets_at,
                Err(_) => now_secs(), // Fallback if peek fails
            };
            let retry_after = resets_at.saturating_sub(now_secs());
            Reply::json(429, json!({"error": "budget_exhausted", "retry_after": retry_after}))
        }
        Err(_) => Reply::err(503, "budget_unavailable"),
    }
}

fn authorize_perm(route: &Route, action: &str) -> Result<String, Reply> {
    let perm = Permission { target: "docs".to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &perm) {
        Ok(p) => Ok(p.subject),
        Err(err) => {
            use crate::bindings::auth::identity::types::AuthError;
            let reply = match err {
                AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
                AuthError::BackendUnavailable(_) | AuthError::Internal(_) => {
                    Reply::err(503, "auth_unavailable")
                }
                _ => Reply::err(401, "unauthenticated"),
            };
            Err(reply)
        }
    }
}
