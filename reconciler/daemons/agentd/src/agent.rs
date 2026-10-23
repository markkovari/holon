//! The loop one task runs: ask the model, run the tools it asks for (after a
//! human says yes, unless the session auto-approves them), hand the results
//! back, until it answers without tools or runs out of turns or budget.
//!
//! With a park (ADR-0113) the wait for a human is not a future held here. The loop writes down
//! where it stands, parks the approval with `comp-park` and returns, and a loop of its own begins
//! again from that checkpoint when the answer is delivered, in this process or in the one that
//! replaced it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use comp_fuel::cost::{cost_usd_micros, micros_at};

use super::model::{Call, Msg, Provider, ToolResult};
use super::park::Park;
use super::session::{Answer, BeginPark, Checkpoint, Session, Started};
use super::tools;
use super::vcs::Vcs;
use super::wire::{new_id, ApprovalScope, Decision, FailCode, Payload, Usage};

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
    /// `comp-park`, when the wait for a human is to be parked (ADR-0113). `None`: it is a future
    /// held here, and a restart ends the task, as it always did.
    pub park: Option<Park>,
    /// `comp-vcs`, to record each `write_file` in (ADR-0121). `None`: edits are not recorded, and
    /// a session cannot be rewound.
    pub vcs: Option<Vcs>,
}

/// How a task's loop begins: from its prompt, or from the answer to the approval it parked on.
pub enum Start {
    Prompt(String),
    Resume { cp: Checkpoint, answer: Answer },
}

/// How a loop ended: with the task's outcome, or by parking on a human, which leaves the task
/// running with no loop (or was cancelled out from under, which leaves nothing to end).
enum Done {
    Ended(Result<String, (FailCode, String)>),
    Parked,
}

/// Run a task that has just become the running one: one `begin` started on an idle session,
/// or one promoted from the queue by the end of the task before it (`run`'s tail, or a cancel).
/// A plain function that spawns, not an `async fn`: `run` calls it when it ends, and `run`
/// spawning `run` directly would make its own future a type that contains itself.
pub fn launch(sess: Arc<Session>, ctx: Arc<Ctx>, started: Started) {
    spawn(sess, ctx, started.task.id, Start::Prompt(started.prompt));
}

/// Go on with a task from the approval it parked on, now that it has an answer (ADR-0113).
pub fn resume(sess: Arc<Session>, ctx: Arc<Ctx>, cp: Checkpoint, answer: Answer) {
    let id = cp.task_id.clone();
    spawn(sess, ctx, id, Start::Resume { cp, answer });
}

fn spawn(sess: Arc<Session>, ctx: Arc<Ctx>, id: String, start: Start) {
    let handle = tokio::spawn(run(sess.clone(), ctx, id.clone(), start));
    sess.attach(&id, handle);
}

pub async fn run(sess: Arc<Session>, ctx: Arc<Ctx>, task_id: String, start: Start) {
    use futures::FutureExt;
    // A panic must still end the task, or the session stays busy forever.
    let done = std::panic::AssertUnwindSafe(turns(&sess, &ctx, &task_id, start))
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            Done::Ended(Err((FailCode::Internal, "the agent loop panicked".into())))
        });
    let outcome = match done {
        // The task is still running, and waiting for a human: nothing ends here.
        Done::Parked => return,
        Done::Ended(outcome) => outcome,
    };
    sess.save_history(&sess.history.lock().await);
    // The next queued task starts in the breath this one ends: `finish` hands it back
    // under the same lock, and it is launched here (ADR-0112).
    if let Some(next) = sess.finish(&task_id, outcome) {
        launch(sess, ctx, next);
    }
}

async fn turns(sess: &Session, ctx: &Ctx, task: &str, start: Start) -> Done {
    let mut history = sess.history.lock().await;
    let (first, mut resume) = match start {
        Start::Prompt(prompt) => {
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
            (0, None)
        }
        // Where it stood: the conversation is already what it was (it was written when the
        // task parked), so the loop goes on from the step it was in, with the answer.
        Start::Resume { cp, answer } => (
            cp.turn,
            Some(Step {
                calls: cp.calls,
                next: cp.next,
                results: cp.results,
                answer: Some(answer),
            }),
        ),
    };
    for turn in first..ctx.max_turns {
        let mut step = match resume.take() {
            Some(step) => step,
            None => match model_turn(sess, ctx, task, &mut history).await {
                Model::Calls(calls) => Step { calls, next: 0, results: Vec::new(), answer: None },
                Model::Again => continue,
                Model::Ended(outcome) => return Done::Ended(outcome),
            },
        };
        match tools_turn(sess, ctx, task, turn, &history, &mut step).await {
            Tools::Ran => {}
            Tools::Parked => return Done::Parked,
            Tools::Failed(code, message) => return Done::Ended(Err((code, message))),
        }
        history.push(Msg::ToolResults(step.results));
        // The tool-round boundary: every call the model asked for has an answer, and the
        // next model call has not begun. A steer is read here, never mid-tool.
        apply_steers(sess, task, &mut history, sess.take_steers(task));
    }
    Done::Ended(Err((FailCode::MaxTurns, format!("no answer after {} model turns", ctx.max_turns))))
}

/// What the model said in one turn.
enum Model {
    /// It asked for these tools.
    Calls(Vec<Call>),
    /// It answered, and a steer that arrived while it wrote gives it another turn.
    Again,
    /// The task is over.
    Ended(Result<String, (FailCode, String)>),
}

async fn model_turn(sess: &Session, ctx: &Ctx, task: &str, history: &mut Vec<Msg>) -> Model {
    if sess.remaining() <= 0 {
        return Model::Ended(Err((FailCode::BudgetExceeded, "session budget is spent".into())));
    }
    let message_id = new_id("msg");
    let mut on_text = |t: &str| {
        sess.emit(task, Payload::TextDelta { message_id: message_id.clone(), text: t.into() })
    };
    let turn = match ctx.provider.turn(&ctx.http, &sess.model, history, &mut on_text).await {
        Ok(turn) => turn,
        Err(e) => return Model::Ended(Err((FailCode::ModelError, format!("{e:#}")))),
    };

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
    let Some((_, remaining)) = sess.charge(task, delta) else {
        return Model::Ended(Err((FailCode::Cancelled, "the task is not running any more".into())));
    };
    history.push(Msg::Assistant { text: turn.text.clone(), calls: turn.calls.clone() });
    if remaining < 0 {
        return Model::Ended(Err((
            FailCode::BudgetExceeded,
            "this call crossed the session budget".into(),
        )));
    }
    if turn.calls.is_empty() {
        // The model has answered. A steer that arrived while it was writing is new
        // input, not an interruption of an answer already given, so the task takes
        // another turn on it rather than finishing past it; with none, steers are
        // refused from here on (ADR-0107).
        let steers = sess.finishing(task);
        if steers.is_empty() {
            return Model::Ended(Ok(turn.text));
        }
        apply_steers(sess, task, history, steers);
        return Model::Again;
    }
    Model::Calls(turn.calls)
}

/// One step of a task: the calls the model asked for, how many have run, and what they returned.
/// A step that was parked comes back with the answer to the call it was waiting on.
struct Step {
    calls: Vec<Call>,
    next: usize,
    results: Vec<ToolResult>,
    answer: Option<Answer>,
}

enum Tools {
    /// Every call has a result.
    Ran,
    Parked,
    Failed(FailCode, String),
}

/// Run the calls of `step` from where it stands.
async fn tools_turn(
    sess: &Session,
    ctx: &Ctx,
    task: &str,
    turn: u32,
    history: &[Msg],
    step: &mut Step,
) -> Tools {
    while step.next < step.calls.len() {
        let call = step.calls[step.next].clone();
        // The call a parked answer is for was announced before it parked, and is not again.
        let answer = match step.answer.take() {
            Some(answer) => Some(answer),
            None => {
                sess.emit(
                    task,
                    Payload::ToolCallStarted {
                        call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        input: call.input.clone(),
                    },
                );
                // Asked at each call, not once for the step: an approval a person gave for the
                // first call of a tool in this step already covers the second (ADR-0114).
                if sess.runs_without_asking(task, &call.name) {
                    None
                } else {
                    match ask(sess, ctx, task, turn, history, step, &call).await {
                        Asked::Answer(answer) => Some(answer),
                        Asked::Parked => return Tools::Parked,
                        Asked::Failed(message) => {
                            return Tools::Failed(FailCode::Internal, message)
                        }
                    }
                }
            }
        };
        // What the file held before, for the record of the change (ADR-0121).
        let recording = match (&ctx.vcs, call.name.as_str()) {
            (Some(_), "write_file") => tools::target(&sess.workspace, &call.input)
                .map(|(rel, full)| (rel, std::fs::read(full).ok())),
            _ => None,
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
        if let (Some(vcs), Some((path, before)), false) = (&ctx.vcs, recording, out.is_error) {
            let after =
                call.input.get("content").and_then(serde_json::Value::as_str).unwrap_or_default();
            let by = holon_vcs::model::Agent {
                id: format!("agentd:{}/{task}/{}", sess.id, call.id),
                goal: None,
                model: Some(sess.model.clone()),
            };
            match vcs.record(&sess.id, &path, before.as_deref(), after.as_bytes(), by).await {
                Ok(ops) => {
                    sess.emit(task, Payload::FileChanged { call_id: call.id.clone(), path, ops })
                }
                // The edit is made; only its record, and so a rewind past it, is missing.
                Err(e) => eprintln!("comp-agentd: {}: recording {path}: {e}", sess.id),
            }
        }
        sess.emit(
            task,
            Payload::ToolCallFinished {
                call_id: call.id.clone(),
                output: out.output.clone(),
                is_error: out.is_error,
                duration_ms: started.elapsed().as_millis() as u64,
            },
        );
        step.results.push(ToolResult {
            call_id: call.id,
            output: out.output,
            is_error: out.is_error,
        });
        step.next += 1;
    }
    Tools::Ran
}

enum Asked {
    Answer(Answer),
    /// Parked on it: the loop ends here, and a new one begins from the answer.
    Parked,
    Failed(String),
}

/// Ask a human whether `call` may run: wait for them here, or, with a park, park the wait.
async fn ask(
    sess: &Session,
    ctx: &Ctx,
    task: &str,
    turn: u32,
    history: &[Msg],
    step: &Step,
    call: &Call,
) -> Asked {
    let Some(park) = &ctx.park else {
        return Asked::Answer(approval(sess, ctx, task, &call.id, &call.name, &call.input).await);
    };
    // Written down first, because the answer may come to a process that was not here when the
    // question was asked: the conversation, then where in the step it stands.
    let deadline = ctx.approval_timeout.map(Park::deadline_in);
    let cp = Checkpoint {
        task_id: task.to_string(),
        turn,
        calls: step.calls.clone(),
        next: step.next,
        results: step.results.clone(),
        deadline,
        ticket: None,
    };
    if let Err(e) = sess.save_history_checked(history) {
        return Asked::Failed(format!("could not save the conversation to park on: {e:#}"));
    }
    match sess.begin_park(cp) {
        BeginPark::Parked => {}
        // Cancelled out from under it: there is nothing to wait for and nothing to end.
        BeginPark::Gone => return Asked::Parked,
        BeginPark::Unsaved(e) => {
            return Asked::Failed(format!("could not save a checkpoint to park on: {e}"))
        }
    }
    let expires_at = deadline
        .and_then(|ms| jiff::Timestamp::from_millisecond(ms as i64).ok())
        .map(|t| t.to_string());
    sess.emit(
        task,
        Payload::ToolApprovalRequired {
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            input: call.input.clone(),
            expires_at,
        },
    );
    let what = format!("approve {} in session {}", call.name, sess.id);
    match park.park_with_retries(&sess.id, &call.id, &what, deadline).await {
        Ok(ticket) => {
            sess.set_ticket(&call.id, &ticket);
            Asked::Parked
        }
        Err(e) => {
            sess.abort_park(&call.id);
            Asked::Failed(format!("could not park the approval of {}: {e}", call.name))
        }
    }
}

/// Each steer joins the conversation as a user message, in the order it arrived, and the
/// log says where: that is the point a replay of the stream sees it enter.
fn apply_steers(sess: &Session, task: &str, history: &mut Vec<Msg>, steers: Vec<(String, String)>) {
    for (steer_id, text) in steers {
        history.push(Msg::User(text));
        sess.emit(task, Payload::TaskSteered { steer_id });
    }
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
    let rx = sess.await_approval(task, call_id);
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
        scope: ApprovalScope::Once,
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
    sess.resolved(task, call_id, &answer);
    answer
}
