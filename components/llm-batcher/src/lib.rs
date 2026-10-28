//! `llm-batcher` — Batch coalescer and queue drainer for LLMs.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

pub mod coalesce;

use std::sync::Mutex;
use coalesce::BatchItem;
#[cfg_attr(not(target_arch = "wasm32"), allow(unused_imports))]
use coalesce::TaskOutput;

#[derive(Default)]
pub struct QueueState {
    pub pending: Vec<BatchItem>,
    pub completed: std::collections::HashMap<String, String>,
}

static STATE: Mutex<Option<QueueState>> = Mutex::new(None);

pub fn with_state<R>(f: impl FnOnce(&mut QueueState) -> R) -> R {
    let mut guard = STATE.lock().unwrap();
    if guard.is_none() {
        *guard = Some(QueueState::default());
    }
    f(guard.as_mut().unwrap())
}

pub struct Component;

#[cfg(target_arch = "wasm32")]
use bindings::exports::holon::llm_batcher::batch_queue::{BatchError, CompletedTask, Guest};
#[cfg(target_arch = "wasm32")]
use bindings::llm::inference::inference as llm;
#[cfg(target_arch = "wasm32")]
use bindings::wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

#[cfg(target_arch = "wasm32")]
impl Guest for Component {
    fn enqueue(
        id: String,
        prompt: String,
        system: String,
        _model: String,
    ) -> Result<String, BatchError> {
        if prompt.trim().is_empty() {
            return Err(BatchError::InvalidTask("empty prompt".into()));
        }
        let task_id = if id.is_empty() {
            format!("batch-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0))
        } else {
            id
        };

        let sys = if system.is_empty() { None } else { Some(system) };
        with_state(|state| {
            state.pending.push(BatchItem {
                id: task_id.clone(),
                prompt,
                system: sys,
            });
        });

        Ok(task_id)
    }

    fn drain(max_items: u32) -> Result<Vec<CompletedTask>, BatchError> {
        let items: Vec<BatchItem> = with_state(|state| {
            let limit = (max_items as usize).min(state.pending.len());
            state.pending.drain(0..limit).collect()
        });

        if items.is_empty() {
            return Err(BatchError::QueueEmpty);
        }

        let coalesced_user_prompt = coalesce::build_coalesced_prompt(&items);
        let sys_prompt = coalesce::system_prompt().to_string();

        let opts = llm::Options {
            model: String::new(), // Use router default (Tier 1 free pool)
            temperature: 100,     // Low temperature for reliable structured JSON
            max_tokens: 4096,
            stop: Vec::new(),
            seed: 0,
        };

        let resp = llm::complete(&coalesced_user_prompt, &sys_prompt, &opts)
            .map_err(|e| BatchError::BackendUnavailable(format!("{e:?}")))?;

        let parsed_outputs = coalesce::parse_coalesced_response(&resp.text)
            .map_err(BatchError::BackendUnavailable)?;

        let mut out = Vec::new();
        with_state(|state| {
            for TaskOutput { id, text } in parsed_outputs {
                state.completed.insert(id.clone(), text.clone());
                out.push(CompletedTask { id, text });
            }
        });

        Ok(out)
    }

    fn poll(id: String) -> Result<Option<String>, BatchError> {
        with_state(|state| {
            if let Some(text) = state.completed.get(&id) {
                Ok(Some(text.clone()))
            } else if state.pending.iter().any(|item| item.id == id) {
                Ok(None)
            } else {
                Err(BatchError::NotFound)
            }
        })
    }
}

#[cfg(target_arch = "wasm32")]
impl bindings::exports::wasi::http::incoming_handler::Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".into());
        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);

        let (status, body_bytes) = if path.starts_with("/api/batch/drain") {
            match <Component as Guest>::drain(20) {
                Ok(results) => {
                    let json = serde_json::json!({
                        "ok": true,
                        "drained": results.len(),
                        "results": results.iter().map(|r| serde_json::json!({"id": r.id, "text": r.text})).collect::<Vec<_>>()
                    });
                    (200, json.to_string().into_bytes())
                }
                Err(BatchError::QueueEmpty) => {
                    (200, b"{\"ok\":true,\"drained\":0,\"results\":[]}".to_vec())
                }
                Err(e) => {
                    (500, format!("{{\"error\": \"{:?}\"}}", e).into_bytes())
                }
            }
        } else if let Some(id) = path.strip_prefix("/api/batch/") {
            match <Component as Guest>::poll(id.to_string()) {
                Ok(Some(text)) => {
                    let json = serde_json::json!({ "id": id, "status": "completed", "text": text });
                    (200, json.to_string().into_bytes())
                }
                Ok(None) => {
                    let json = serde_json::json!({ "id": id, "status": "pending" });
                    (200, json.to_string().into_bytes())
                }
                Err(_) => (404, b"{\"error\":\"not_found\"}".to_vec()),
            }
        } else {
            (200, b"{\"service\":\"llm-batcher\",\"endpoints\":[\"POST /api/batch\",\"GET /api/batch/{id}\",\"POST /api/batch/drain\"]}".to_vec())
        };

        let resp = OutgoingResponse::new(headers);
        resp.set_status_code(status).unwrap();
        let body = resp.body().unwrap();
        ResponseOutparam::set(response_out, Ok(resp));
        let stream = body.write().unwrap();
        let _ = stream.blocking_write_and_flush(&body_bytes);
        drop(stream);
        let _ = OutgoingBody::finish(body, None);
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
