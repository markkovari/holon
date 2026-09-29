//! `api/holon/v1/agent.proto` over tonic: the same `service.rs` calls as
//! REST, translated to and from the generated types. Served on the REST
//! port (HTTP/2), plain gRPC and gRPC-web both, so a Connect client using
//! either of those protocols reaches it.

use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, StreamExt};
use prost_types::{value::Kind, ListValue, Struct, Timestamp};
use serde_json::Value;
use tonic::{Request, Response, Status};

use super::service::{Daemon, Fail};
use super::wire;

pub mod pb {
    tonic::include_proto!("holon.v1");
}

use pb::agent_event::Payload as P;
use pb::agent_service_server::{AgentService, AgentServiceServer};

pub struct Grpc(pub Arc<Daemon>);

pub fn server(d: Arc<Daemon>) -> AgentServiceServer<Grpc> {
    AgentServiceServer::new(Grpc(d))
}

fn status(f: Fail) -> Status {
    match f {
        Fail::NotFound(d) => Status::not_found(d),
        Fail::Invalid(_, d) => Status::invalid_argument(d),
        Fail::Conflict(_, d) => Status::failed_precondition(d),
        Fail::Internal(d) => Status::internal(d),
    }
}

// ---- wire -> proto ----

fn ts(rfc3339: &str) -> Option<Timestamp> {
    let t: jiff::Timestamp = rfc3339.parse().ok()?;
    Some(Timestamp { seconds: t.as_second(), nanos: t.subsec_nanosecond() })
}

fn usage(u: wire::Usage) -> pb::Usage {
    pb::Usage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_read_tokens: u.cache_read_tokens,
        cache_write_tokens: u.cache_write_tokens,
        cost_usd_micros: u.cost_usd_micros,
    }
}

fn session(s: wire::Session) -> pb::Session {
    let status = match s.status {
        wire::SessionStatus::Idle => pb::SessionStatus::Idle,
        wire::SessionStatus::Running => pb::SessionStatus::Running,
        wire::SessionStatus::AwaitingApproval => pb::SessionStatus::AwaitingApproval,
        wire::SessionStatus::Closed => pb::SessionStatus::Closed,
    };
    pb::Session {
        id: s.id,
        model: s.model,
        workspace_dir: s.workspace_dir,
        status: status as i32,
        usage: Some(usage(s.usage)),
        max_budget_usd_micros: s.max_budget_usd_micros,
        auto_approve_tools: s.auto_approve_tools,
        labels: s.labels.into_iter().collect(),
        created_at: ts(&s.created_at),
        active_task_id: s.active_task_id.unwrap_or_default(),
    }
}

fn task(t: wire::Task) -> pb::Task {
    let status = match t.status {
        wire::TaskStatus::Running => pb::TaskStatus::Running,
        wire::TaskStatus::Completed => pb::TaskStatus::Completed,
        wire::TaskStatus::Failed => pb::TaskStatus::Failed,
    };
    pb::Task {
        id: t.id,
        session_id: t.session_id,
        status: status as i32,
        created_at: ts(&t.created_at),
    }
}

fn decision(d: wire::Decision) -> i32 {
    match d {
        wire::Decision::Approve => pb::ApprovalDecision::Approve as i32,
        wire::Decision::Deny => pb::ApprovalDecision::Deny as i32,
    }
}

/// A JSON object as a `google.protobuf.Struct`; anything else is wrapped
/// under `"value"`, since a Struct must be an object.
fn to_struct(v: Value) -> Struct {
    match v {
        Value::Object(m) => {
            Struct { fields: m.into_iter().map(|(k, v)| (k, to_value(v))).collect() }
        }
        other => Struct { fields: [("value".to_string(), to_value(other))].into() },
    }
}

fn to_value(v: Value) -> prost_types::Value {
    let kind = match v {
        Value::Null => Kind::NullValue(0),
        Value::Bool(b) => Kind::BoolValue(b),
        Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or_default()),
        Value::String(s) => Kind::StringValue(s),
        Value::Array(a) => {
            Kind::ListValue(ListValue { values: a.into_iter().map(to_value).collect() })
        }
        Value::Object(_) => Kind::StructValue(to_struct(v)),
    };
    prost_types::Value { kind: Some(kind) }
}

pub fn event(e: wire::Event) -> pb::AgentEvent {
    use wire::Payload as W;
    let payload = match e.payload {
        W::TaskStarted { prompt } => P::TaskStarted(pb::TaskStarted { prompt }),
        W::TextDelta { message_id, text } => P::TextDelta(pb::TextDelta { message_id, text }),
        W::ToolCallStarted { call_id, tool_name, input } => {
            P::ToolCallStarted(pb::ToolCallStarted {
                call_id,
                tool_name,
                input: Some(to_struct(input)),
            })
        }
        W::ToolApprovalRequired { call_id, tool_name, input, expires_at } => {
            P::ToolApprovalRequired(pb::ToolApprovalRequired {
                call_id,
                tool_name,
                input: Some(to_struct(input)),
                expires_at: expires_at.as_deref().and_then(ts),
            })
        }
        W::ToolApprovalResolved { call_id, decision: d, decided_by } => {
            P::ToolApprovalResolved(pb::ToolApprovalResolved {
                call_id,
                decision: decision(d),
                decided_by,
            })
        }
        W::ToolCallFinished { call_id, output, is_error, duration_ms } => {
            P::ToolCallFinished(pb::ToolCallFinished { call_id, output, is_error, duration_ms })
        }
        W::UsageUpdated { delta, session_total, budget_remaining_usd_micros } => {
            P::UsageUpdated(pb::UsageUpdated {
                delta: Some(usage(delta)),
                session_total: Some(usage(session_total)),
                budget_remaining_usd_micros,
            })
        }
        W::TaskCompleted { result, task_usage } => {
            P::TaskCompleted(pb::TaskCompleted { result, task_usage: Some(usage(task_usage)) })
        }
        W::TaskFailed { code, message, task_usage } => {
            let code = match code {
                wire::FailCode::BudgetExceeded => pb::TaskFailureCode::BudgetExceeded,
                wire::FailCode::Cancelled => pb::TaskFailureCode::Cancelled,
                wire::FailCode::ModelError => pb::TaskFailureCode::ModelError,
                wire::FailCode::MaxTurns => pb::TaskFailureCode::MaxTurns,
                wire::FailCode::Internal => pb::TaskFailureCode::Internal,
            };
            P::TaskFailed(pb::TaskFailed {
                code: code as i32,
                message,
                task_usage: Some(usage(task_usage)),
            })
        }
    };
    pb::AgentEvent {
        seq: e.seq,
        session_id: e.session_id,
        task_id: e.task_id,
        time: ts(&e.time),
        payload: Some(payload),
    }
}

#[tonic::async_trait]
impl AgentService for Grpc {
    async fn create_session(
        &self,
        r: Request<pb::CreateSessionRequest>,
    ) -> Result<Response<pb::Session>, Status> {
        let r = r.into_inner();
        let req = wire::CreateSession {
            model: r.model,
            workspace_dir: r.workspace_dir,
            max_budget_usd_micros: r.max_budget_usd_micros,
            auto_approve_tools: r.auto_approve_tools,
            labels: r.labels.into_iter().collect(),
        };
        self.0.create_session(req).map(|s| Response::new(session(s))).map_err(status)
    }

    async fn get_session(
        &self,
        r: Request<pb::GetSessionRequest>,
    ) -> Result<Response<pb::Session>, Status> {
        self.0
            .get_session(&r.into_inner().session_id)
            .map(|s| Response::new(session(s)))
            .map_err(status)
    }

    async fn close_session(
        &self,
        r: Request<pb::CloseSessionRequest>,
    ) -> Result<Response<pb::Session>, Status> {
        self.0
            .close_session(&r.into_inner().session_id)
            .map(|s| Response::new(session(s)))
            .map_err(status)
    }

    async fn send_task(
        &self,
        r: Request<pb::SendTaskRequest>,
    ) -> Result<Response<pb::Task>, Status> {
        let r = r.into_inner();
        let key = Some(r.idempotency_key);
        self.0
            .send_task(&r.session_id, r.prompt, key)
            .map(|t| Response::new(task(t)))
            .map_err(status)
    }

    async fn cancel_task(
        &self,
        r: Request<pb::CancelTaskRequest>,
    ) -> Result<Response<pb::Task>, Status> {
        let r = r.into_inner();
        self.0
            .cancel_task(&r.session_id, &r.task_id)
            .map(|t| Response::new(task(t)))
            .map_err(status)
    }

    type StreamEventsStream = Pin<Box<dyn Stream<Item = Result<pb::AgentEvent, Status>> + Send>>;

    async fn stream_events(
        &self,
        r: Request<pb::StreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        let r = r.into_inner();
        let s = self.0.events(&r.session_id, r.after_seq).map_err(status)?;
        Ok(Response::new(Box::pin(s.map(|e| Ok(event(e))))))
    }

    async fn submit_approval(
        &self,
        r: Request<pb::SubmitApprovalRequest>,
    ) -> Result<Response<pb::SubmitApprovalResponse>, Status> {
        let r = r.into_inner();
        let d = match pb::ApprovalDecision::try_from(r.decision) {
            Ok(pb::ApprovalDecision::Approve) => wire::Decision::Approve,
            Ok(pb::ApprovalDecision::Deny) => wire::Decision::Deny,
            _ => return Err(Status::invalid_argument("decision must be APPROVE or DENY")),
        };
        let req = wire::SubmitApproval { decision: d, reason: r.reason, approver: r.approver };
        self.0
            .submit_approval(&r.session_id, &r.call_id, req)
            .map(|a| {
                Response::new(pb::SubmitApprovalResponse {
                    call_id: a.call_id,
                    decision: decision(a.decision),
                })
            })
            .map_err(status)
    }
}
