//! OpenTelemetry export: every run becomes an `invoke_agent` span, each model
//! call a child `chat` span and each tool call a child `execute_tool` span,
//! named and attributed per the OpenTelemetry GenAI semantic conventions.
//! Runs woken by another run are children of the tool-call span that woke them
//! (W3C Trace Context), so one trace follows a whole chain across agents.
//!
//! Wire format is OTLP/HTTP with JSON encoding — `POST <endpoint>/v1/traces`
//! — which every collector, Jaeger and Tempo accept. `request` builds the same
//! document for the admin API, so a trace can be fetched and opened in any
//! OTLP-aware tool without a collector at all.
//! <https://opentelemetry.io/docs/specs/otlp/#otlphttp>
//! <https://opentelemetry.io/docs/specs/semconv/gen-ai/>

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{json, Value};

use crate::store::{RunRecord, Status, Step};

const INTERNAL: u8 = 1;
const SERVER: u8 = 2;
const CLIENT: u8 = 3;
const CONSUMER: u8 = 5;

fn s(key: &str, v: impl Into<String>) -> Value {
    json!({"key": key, "value": {"stringValue": v.into()}})
}

/// OTLP/JSON carries 64-bit integers as decimal strings.
fn i(key: &str, v: u64) -> Value {
    json!({"key": key, "value": {"intValue": v.to_string()}})
}

fn nanos(ms: u64) -> String {
    (ms.saturating_mul(1_000_000)).to_string()
}

fn status(ok: bool, message: &str) -> Value {
    if ok {
        json!({"code": 1})
    } else {
        json!({"code": 2, "message": message})
    }
}

fn provider(model: &str) -> &str {
    model.split('/').next().unwrap_or("")
}

fn spans_for(r: &RunRecord) -> Vec<Value> {
    if r.trace_id.is_empty() || r.span_id.is_empty() {
        return Vec::new(); // a run from before tracing existed
    }
    let kind = if r.trigger == "http" {
        SERVER
    } else if r.trigger.starts_with("event:") || r.trigger.starts_with("store:") {
        CONSUMER
    } else {
        INTERNAL
    };
    let ok = r.status == Status::Ok;
    let mut run = json!({
        "traceId": r.trace_id,
        "spanId": r.span_id,
        "parentSpanId": r.parent_span_id.clone().unwrap_or_default(),
        "name": format!("invoke_agent {}", r.agent),
        "kind": kind,
        "startTimeUnixNano": nanos(r.started_ms),
        "endTimeUnixNano": nanos(r.finished_ms.max(r.started_ms)),
        "attributes": [
            s("gen_ai.operation.name", "invoke_agent"),
            s("gen_ai.agent.name", r.agent.clone()),
            i("gen_ai.usage.input_tokens", r.tokens_in),
            i("gen_ai.usage.output_tokens", r.tokens_out),
            s("holon.trigger", r.trigger.clone()),
            s("holon.run.id", r.id.clone()),
            s("holon.run.status", format!("{:?}", r.status).to_lowercase()),
            i("holon.hops", u64::from(r.hops)),
            s("holon.chain", r.chain.join(" > ")),
        ],
        "status": status(ok, &r.answer),
    });
    if !r.model.is_empty() {
        run["attributes"].as_array_mut().unwrap().push(s("gen_ai.request.model", r.model.clone()));
    }
    let mut out = vec![run];
    for step in &r.steps {
        match step {
            Step::Model { span_id, t0, t1, tokens_in, tokens_out, .. } if !span_id.is_empty() => {
                out.push(json!({
                    "traceId": r.trace_id,
                    "spanId": span_id,
                    "parentSpanId": r.span_id,
                    "name": format!("chat {}", r.model.rsplit('/').next().unwrap_or("")),
                    "kind": CLIENT,
                    "startTimeUnixNano": nanos(*t0),
                    "endTimeUnixNano": nanos((*t1).max(*t0)),
                    "attributes": [
                        s("gen_ai.operation.name", "chat"),
                        s("gen_ai.system", provider(&r.model)),
                        s("gen_ai.request.model", r.model.clone()),
                        i("gen_ai.usage.input_tokens", *tokens_in),
                        i("gen_ai.usage.output_tokens", *tokens_out),
                    ],
                    "status": {"code": 1},
                }));
            }
            Step::Tool { name, result, error, approved, span_id, t0, t1, .. }
                if !span_id.is_empty() =>
            {
                let mut attrs = vec![
                    s("gen_ai.operation.name", "execute_tool"),
                    s("gen_ai.tool.name", name.clone()),
                ];
                if let Some(a) = approved {
                    attrs.push(s("holon.tool.approved", a.to_string()));
                }
                if *error {
                    attrs.push(s("error.type", "tool_error"));
                }
                out.push(json!({
                    "traceId": r.trace_id,
                    "spanId": span_id,
                    "parentSpanId": r.span_id,
                    "name": format!("execute_tool {name}"),
                    "kind": INTERNAL,
                    "startTimeUnixNano": nanos(*t0),
                    "endTimeUnixNano": nanos((*t1).max(*t0)),
                    "attributes": attrs,
                    "status": status(!*error, result),
                }));
            }
            _ => {}
        }
    }
    out
}

/// An OTLP `ExportTraceServiceRequest` (JSON) for these runs.
pub fn request(runs: &[RunRecord]) -> Value {
    let spans: Vec<Value> = runs.iter().flat_map(spans_for).collect();
    json!({
        "resourceSpans": [{
            "resource": {"attributes": [
                s("service.name", "holon-agent-runtime"),
                s("service.version", env!("CARGO_PKG_VERSION")),
            ]},
            "scopeSpans": [{
                "scope": {"name": "agent-runtime", "version": env!("CARGO_PKG_VERSION")},
                "spans": spans,
            }],
        }],
    })
}

static WARNED: AtomicBool = AtomicBool::new(false);

/// Sends one finished run to the collector on its own thread. Telemetry must
/// never slow or fail a run, so errors are swallowed after one warning.
pub fn export_async(endpoint: String, rec: RunRecord) {
    std::thread::spawn(move || {
        let body = request(std::slice::from_ref(&rec));
        let sent = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .and_then(|c| {
                c.post(format!("{}/v1/traces", endpoint.trim_end_matches('/'))).json(&body).send()
            })
            .map(|r| r.status().is_success());
        if !matches!(sent, Ok(true)) && !WARNED.swap(true, Ordering::Relaxed) {
            eprintln!("agent-runtime: OTLP export to {endpoint} failed ({sent:?}); further failures are silent");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Step;
    use crate::trace::{new_span_id, new_trace_id};

    fn rec() -> RunRecord {
        let (trace, run, model_span, tool_span) =
            (new_trace_id(), new_span_id(), new_span_id(), new_span_id());
        RunRecord {
            id: "a-1-0".into(),
            agent: "logwatch".into(),
            trigger: "schedule: @every 5m".into(),
            input: "go".into(),
            started: 1,
            finished: 3,
            status: Status::Ok,
            answer: "done".into(),
            steps: vec![
                Step::Model {
                    text: "{}".into(),
                    span_id: model_span,
                    t0: 1_000,
                    t1: 1_500,
                    tokens_in: 100,
                    tokens_out: 20,
                },
                Step::Tool {
                    name: "store_put".into(),
                    args: json!({}),
                    result: "stored".into(),
                    error: false,
                    approved: None,
                    span_id: tool_span,
                    t0: 1_500,
                    t1: 1_600,
                },
            ],
            tokens_in: 100,
            tokens_out: 20,
            started_ms: 1_000,
            finished_ms: 3_000,
            model: "anthropic/claude-haiku-4-5".into(),
            trace_id: trace,
            span_id: run,
            parent_span_id: Some(new_span_id()),
            hops: 1,
            chain: vec!["coach|store.rowing".into()],
        }
    }

    #[test]
    fn a_run_is_an_invoke_agent_span_with_chat_and_tool_children() {
        let r = rec();
        let doc = request(std::slice::from_ref(&r));
        let spans = doc["resourceSpans"][0]["scopeSpans"][0]["spans"].as_array().unwrap();
        assert_eq!(spans.len(), 3);
        let names: Vec<&str> = spans.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            ["invoke_agent logwatch", "chat claude-haiku-4-5", "execute_tool store_put"]
        );
        // one trace; children point at the run span; the run points at its parent
        assert!(spans.iter().all(|s| s["traceId"] == r.trace_id.as_str()));
        assert_eq!(spans[1]["parentSpanId"], r.span_id.as_str());
        assert_eq!(spans[2]["parentSpanId"], r.span_id.as_str());
        assert_eq!(spans[0]["parentSpanId"], r.parent_span_id.clone().unwrap().as_str());
        // ids are the standard widths
        assert_eq!(spans[0]["traceId"].as_str().unwrap().len(), 32);
        assert_eq!(spans[0]["spanId"].as_str().unwrap().len(), 16);
        // times are unix nanos as strings
        assert_eq!(spans[0]["startTimeUnixNano"], "1000000000");
        assert_eq!(spans[0]["endTimeUnixNano"], "3000000000");
    }

    #[test]
    fn genai_attributes_and_status_follow_the_conventions() {
        let doc = request(&[rec()]);
        let spans = doc["resourceSpans"][0]["scopeSpans"][0]["spans"].as_array().unwrap();
        let attr = |sp: &Value, k: &str| -> Option<String> {
            sp["attributes"].as_array().unwrap().iter().find(|a| a["key"] == k).map(|a| {
                let v = &a["value"];
                v["stringValue"].as_str().or(v["intValue"].as_str()).unwrap().to_string()
            })
        };
        assert_eq!(attr(&spans[0], "gen_ai.operation.name").as_deref(), Some("invoke_agent"));
        assert_eq!(attr(&spans[0], "gen_ai.agent.name").as_deref(), Some("logwatch"));
        assert_eq!(attr(&spans[1], "gen_ai.operation.name").as_deref(), Some("chat"));
        assert_eq!(attr(&spans[1], "gen_ai.system").as_deref(), Some("anthropic"));
        assert_eq!(attr(&spans[1], "gen_ai.usage.input_tokens").as_deref(), Some("100"));
        assert_eq!(attr(&spans[2], "gen_ai.tool.name").as_deref(), Some("store_put"));
        assert_eq!(spans[0]["status"]["code"], 1);
        assert_eq!(
            doc["resourceSpans"][0]["resource"]["attributes"][0]["value"]["stringValue"],
            "holon-agent-runtime"
        );
    }

    #[test]
    fn failures_and_drops_are_error_spans_and_event_wakeups_are_consumers() {
        let mut r = rec();
        r.status = Status::Dropped;
        r.answer = "dropped: cycle".into();
        r.trigger = "event: deploy".into();
        if let Step::Tool { error, .. } = &mut r.steps[1] {
            *error = true;
        }
        let doc = request(&[r]);
        let spans = doc["resourceSpans"][0]["scopeSpans"][0]["spans"].as_array().unwrap();
        assert_eq!(spans[0]["status"]["code"], 2);
        assert_eq!(spans[0]["status"]["message"], "dropped: cycle");
        assert_eq!(spans[0]["kind"], 5);
        assert_eq!(spans[2]["status"]["code"], 2);
    }

    #[test]
    fn a_run_from_before_tracing_exports_nothing_rather_than_garbage() {
        let mut r = rec();
        r.trace_id.clear();
        assert!(request(&[r])
            .pointer("/resourceSpans/0/scopeSpans/0/spans")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
    }
}
