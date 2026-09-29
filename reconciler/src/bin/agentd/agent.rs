//! The loop one task runs: ask the model, run the tools it asks for (after a
//! human says yes, unless the session auto-approves them), hand the results
//! back, until it answers without tools or runs out of turns or budget.

use std::sync::Arc;
use std::time::{Duration, Instant};

use comp_reconciler::cost::{cost_usd_micros, micros_at};

use super::model::{Msg, Provider, ToolResult};
use super::session::{Answer, Session};
use super::tools;
use super::wire::{new_id, Decision, FailCode, Payload, Usage};

pub struct Ctx {
    pub provider: Provider,
    pub http: reqwest::Client,
    pub max_turns: u32,
    pub approval_timeout: Option<Duration>,
    /// Operator prices that win over `cost.rs`'s table: (substring of the
    /// model id, input, output), cents per million tokens.
    pub prices: Vec<(String, u64, u64)>,
    /// Whether DeepSeek's off-peak price applies; a clock question `cost.rs`
    /// leaves to its caller.
    pub off_peak: fn() -> bool,
}

pub async fn run(sess: Arc<Session>, ctx: Arc<Ctx>, task_id: String, prompt: String) {
    use futures::FutureExt;
    // A panic must still end the task, or the session stays busy forever.
    let outcome = std::panic::AssertUnwindSafe(turns(&sess, &ctx, &task_id, prompt))
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err((FailCode::Internal, "the agent loop panicked".into())));
    sess.save_history(&sess.history.lock().await);
    sess.finish(&task_id, outcome);
}

async fn turns(
    sess: &Session,
    ctx: &Ctx,
    task: &str,
    prompt: String,
) -> Result<String, (FailCode, String)> {
    let mut history = sess.history.lock().await;
    // A task cancelled mid-tool leaves calls with no results, and a provider
    // refuses a conversation like that. Answer them before moving on.
    if let Some(Msg::Assistant { calls, .. }) = history.last() {
        let rs: Vec<ToolResult> = calls
            .iter()
            .map(|c| ToolResult {
                call_id: c.id.clone(),
                output: "cancelled before it ran".into(),
                is_error: true,
            })
            .collect();
        if !rs.is_empty() {
            history.push(Msg::ToolResults(rs));
        }
    }
    history.push(Msg::User(prompt));
    for _ in 0..ctx.max_turns {
        if sess.remaining() <= 0 {
            return Err((FailCode::BudgetExceeded, "session budget is spent".into()));
        }
        let message_id = new_id("msg");
        let mut on_text = |t: &str| {
            sess.emit(task, Payload::TextDelta { message_id: message_id.clone(), text: t.into() })
        };
        let turn = ctx
            .provider
            .turn(&ctx.http, &sess.model, &history, &mut on_text)
            .await
            .map_err(|e| (FailCode::ModelError, format!("{e:#}")))?;

        let t = &turn.tokens;
        // Case-insensitive: `--price qwen=0,0` must match `mlx-community/Qwen3-4B`.
        let model_lc = sess.model.to_lowercase();
        let cost = match ctx.prices.iter().find(|(p, ..)| model_lc.contains(&p.to_lowercase())) {
            Some((_, i, o)) => micros_at(*i, *o, t.input, t.output, t.cache_read, t.cache_write),
            None => cost_usd_micros(
                t.input,
                t.output,
                t.cache_read,
                t.cache_write,
                &sess.model,
                (ctx.off_peak)(),
            ),
        };
        let delta = Usage {
            input_tokens: t.input,
            output_tokens: t.output,
            cache_read_tokens: t.cache_read,
            cache_write_tokens: t.cache_write,
            cost_usd_micros: cost as i64,
        };
        let (total, remaining) = sess.charge(task, delta);
        sess.emit(
            task,
            Payload::UsageUpdated {
                delta,
                session_total: total,
                budget_remaining_usd_micros: remaining,
            },
        );
        history.push(Msg::Assistant { text: turn.text.clone(), calls: turn.calls.clone() });
        if remaining < 0 {
            return Err((FailCode::BudgetExceeded, "this call crossed the session budget".into()));
        }
        if turn.calls.is_empty() {
            return Ok(turn.text);
        }

        let mut results = Vec::new();
        for call in turn.calls {
            sess.emit(
                task,
                Payload::ToolCallStarted {
                    call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    input: call.input.clone(),
                },
            );
            let answer = if sess.auto_approve.contains(&call.name) {
                None
            } else {
                Some(approval(sess, ctx, task, &call.id, &call.name, &call.input).await)
            };
            let started = Instant::now();
            let out = match answer {
                Some(Answer { decision: Decision::Deny, reason, .. }) => tools::Outcome {
                    output: if reason.is_empty() {
                        "denied by a human".into()
                    } else {
                        format!("denied by a human: {reason}")
                    },
                    is_error: true,
                },
                _ => tools::call(&sess.workspace, &call.name, &call.input).await,
            };
            sess.emit(
                task,
                Payload::ToolCallFinished {
                    call_id: call.id.clone(),
                    output: out.output.clone(),
                    is_error: out.is_error,
                    duration_ms: started.elapsed().as_millis() as u64,
                },
            );
            results.push(ToolResult {
                call_id: call.id,
                output: out.output,
                is_error: out.is_error,
            });
        }
        history.push(Msg::ToolResults(results));
    }
    Err((FailCode::MaxTurns, format!("no answer after {} model turns", ctx.max_turns)))
}

/// Pause for a human. A timeout is a denial, decided by "timeout".
async fn approval(
    sess: &Session,
    ctx: &Ctx,
    task: &str,
    call_id: &str,
    tool_name: &str,
    input: &serde_json::Value,
) -> Answer {
    let rx = sess.await_approval(call_id);
    let expires_at = ctx
        .approval_timeout
        .and_then(|d| jiff::Timestamp::now().checked_add(d).ok())
        .map(|t| t.to_string());
    sess.emit(
        task,
        Payload::ToolApprovalRequired {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            input: input.clone(),
            expires_at,
        },
    );
    let timed_out = || Answer {
        decision: Decision::Deny,
        reason: "nobody answered in time".into(),
        decided_by: "timeout".into(),
    };
    let answer = match ctx.approval_timeout {
        None => rx.await.unwrap_or_else(|_| timed_out()),
        Some(d) => match tokio::time::timeout(d, rx).await {
            Ok(r) => r.unwrap_or_else(|_| timed_out()),
            Err(_) => {
                sess.drop_approval(call_id);
                timed_out()
            }
        },
    };
    sess.emit(
        task,
        Payload::ToolApprovalResolved {
            call_id: call_id.into(),
            decision: answer.decision,
            decided_by: answer.decided_by.clone(),
        },
    );
    answer
}
