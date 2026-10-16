//! What a resumer does (ADR-0106), as functions over the engine and two small
//! traits, so the logic is tested against memory and run against JetStream.
//!
//! A resumer has two jobs:
//!
//! * **Deliver.** A message on `PARK_WAKE` names a ticket. [`handle`] reads the
//!   ticket, hands what it holds to a [`Sink`] — the thing that drives the turn —
//!   and only then closes it. Delivery is at least once: the resolution is read
//!   with [`Engine::resolution`], which does not consume, and the ticket is taken
//!   after the sink accepted. A resumer that dies in between redelivers, and the
//!   sink is expected to treat [`Delivery::idempotency_key`] as the identity of
//!   what it was handed. That is the other half of "resuming is somebody else's
//!   job" (ADR-0100): this crate never learns what a turn does with an answer.
//! * **Recover.** [`sweep`] runs [`Engine::recover`] and announces what it
//!   moved. See `recover.rs` for the table.
//!
//! Nothing above `run` sleeps, spawns or opens a socket. `run` (native only) is the
//! loop that does, over a real `PARK_WAKE` consumer.

use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::error::{ParkError, Result};
use crate::model::{
    Agent, CallResult, Interruption, Resolution, SessionId, TicketId, WakeKind, WakeMessage,
};
use crate::recover::{Recovered, RecoveryPolicy, Repaired};
use crate::store::{ParkStore, StoredStatus};

/// What a sink is handed. On the wire (`comp-resume`'s HTTP sink) it is this
/// JSON, one object, `event` naming which of the three:
///
/// ```json
/// {"ticket": "…", "session": "…", "correlation": "…", "event": "answered",
///  "answer": {"ok": true, "body": [42], "detail": null}}
/// {"ticket": "…", "session": "…", "correlation": "…", "event": "interrupted",
///  "interruption": {"reason": "…", "at": 1700000000000, "by": {"id": "comp-resume"}}}
/// {"ticket": "…", "session": "…", "correlation": "…", "event": "redispatch",
///  "attempt": 2, "description": "…"}
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Delivery {
    pub ticket: TicketId,
    pub session: SessionId,
    /// What the turn called the call; how it finds it in its own journal.
    pub correlation: String,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    /// The far side answered, well or badly. `ok: false` is an answer.
    Answered { answer: CallResult },
    /// No answer will come and the call may or may not have happened: tell the
    /// turn, do not repeat the call.
    Interrupted { interruption: Interruption },
    /// A `safe` call went unanswered past its deadline: send it again. The turn
    /// owns the payload; this crate only ever held a description of it.
    Redispatch { attempt: u32, description: String },
}

impl Delivery {
    /// The identity of what this delivery says, stable across redeliveries: the
    /// same answer or interruption is `{ticket}:answered` however many times it
    /// arrives, and each re-dispatch is its own.
    pub fn idempotency_key(&self) -> String {
        match &self.event {
            Event::Answered { .. } => format!("{}:answered", self.ticket),
            Event::Interrupted { .. } => format!("{}:interrupted", self.ticket),
            Event::Redispatch { attempt, .. } => format!("{}:redispatch:{attempt}", self.ticket),
        }
    }
}

/// What a sink says about a [`Delivery`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkReply {
    /// Accepted — now, or earlier (a redelivery it already had).
    Delivered,
    /// There is nobody to deliver to: the session is gone or closed. Retrying
    /// would never end, so the ticket is cancelled instead.
    Gone(String),
    /// Not now. The message comes back after a delay.
    Retry(String),
}

/// Whatever drives the turn.
pub trait Sink: Send + Sync {
    fn deliver(&self, delivery: &Delivery) -> impl Future<Output = SinkReply> + Send;
}

/// What [`handle`] decided about one `PARK_WAKE` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handled {
    /// The sink took it and the ticket is closed.
    Delivered,
    /// Nothing to do: the ticket is gone, already taken or cancelled, or this
    /// message is about a re-dispatch a later one superseded.
    Stale,
    /// The sink said nobody is there; the ticket was cancelled.
    Gone,
    /// Not finished. Nak the message so it returns after [`backoff`].
    Retry(String),
}

impl Handled {
    /// Whether the message may be acknowledged.
    pub fn is_finished(&self) -> bool {
        !matches!(self, Handled::Retry(_))
    }
}

/// How long to wait before a message's `delivered`-th redelivery: a second,
/// doubling, to a ceiling of 256. A sink that is down for an hour is retried a
/// few dozen times, not a few thousand.
pub fn backoff(delivered: u64) -> Duration {
    let exp = delivered.saturating_sub(1).min(8) as u32;
    Duration::from_secs(1u64 << exp)
}

/// One `PARK_WAKE` message, end to end. Errors are the store's (retry the
/// message); everything the sink can say is in [`Handled`].
///
/// A message is a nudge, not the truth: what is done is decided by the ticket
/// as it is now. A `redispatch` message whose `attempt` the ticket has moved
/// past, or a `resolved` one for a ticket already taken, is stale and finished.
pub async fn handle<S: ParkStore, K: Sink>(
    engine: &Engine<S>,
    sink: &K,
    msg: &WakeMessage,
    by: &Agent,
    now_ms: u64,
) -> Result<Handled> {
    let Some((record, _)) = engine.store.get(&msg.ticket).await? else {
        return Ok(Handled::Stale);
    };

    let event = match (&msg.kind, record.status) {
        (WakeKind::Redispatch { attempt }, StoredStatus::Parked)
            if *attempt == record.redispatches =>
        {
            Event::Redispatch { attempt: *attempt, description: record.call.description.clone() }
        }
        (WakeKind::Resolved, StoredStatus::Ready | StoredStatus::Interrupted) => {
            match engine.resolution(&msg.ticket).await? {
                Resolution::Answered(answer) => Event::Answered { answer },
                Resolution::Interrupted(interruption) => Event::Interrupted { interruption },
            }
        }
        _ => return Ok(Handled::Stale),
    };

    let delivery = Delivery {
        ticket: msg.ticket.clone(),
        session: record.session.clone(),
        correlation: record.call.correlation.clone(),
        event,
    };
    match sink.deliver(&delivery).await {
        SinkReply::Delivered => {
            if matches!(delivery.event, Event::Redispatch { .. }) {
                // Nothing to close: the ticket stays `parked` until it answers.
                return Ok(Handled::Delivered);
            }
            match engine.take_ready(&msg.ticket, now_ms).await {
                // Another resumer took it between the sink's accept and now.
                Ok(_) | Err(ParkError::AlreadyClosed(_)) => Ok(Handled::Delivered),
                Err(e) => Err(e),
            }
        }
        SinkReply::Gone(_) => match engine.cancel(&msg.ticket, by.clone(), now_ms).await {
            Ok(()) | Err(ParkError::AlreadyClosed(_)) => Ok(Handled::Gone),
            Err(e) => Err(e),
        },
        SinkReply::Retry(why) => Ok(Handled::Retry(why)),
    }
}

/// Where a recovery pass puts what it says. `WakeStream` is the real one.
pub trait Announcer: Send + Sync {
    fn announce(&self, msg: &WakeMessage) -> impl Future<Output = Result<()>> + Send;
}

/// What one pass did.
#[derive(Debug, Default)]
pub struct Swept {
    /// Index entries a crash had lost, put back.
    pub repaired: Repaired,
    pub recovered: Vec<Recovered>,
    /// Announcements that did not land. The tickets already moved; the next
    /// pass re-announces a resolved one, and a lost `redispatch` costs that
    /// attempt, which is bounded (`RecoveryPolicy::max_redispatches`).
    pub unannounced: Vec<(TicketId, ParkError)>,
}

/// One pass: repair what a crash left unindexed, recover what never answered, then
/// announce each move. Repair comes first so that the tickets the pass is about to act on
/// are the ones a `wake` and a `pending` can also see.
pub async fn sweep<S: ParkStore, A: Announcer>(
    engine: &Engine<S>,
    announcer: &A,
    by: &Agent,
    policy: &RecoveryPolicy,
    now_ms: u64,
) -> Result<Swept> {
    let repaired = engine.repair().await?;
    let recovered = engine.recover(by, policy, now_ms).await?;
    let mut unannounced = Vec::new();
    for r in &recovered {
        if let Some(msg) = r.announcement() {
            if let Err(e) = announcer.announce(&msg).await {
                unannounced.push((r.ticket.clone(), e));
            }
        }
    }
    Ok(Swept { repaired, recovered, unannounced })
}

/// What [`run`] needs beyond an engine, a stream and a sink.
#[cfg(feature = "native")]
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// The durable consumer's name; resumers sharing one share the work.
    pub consumer: String,
    /// How long a delivered message may go unanswered before the server hands
    /// it to another resumer. Must outlast one sink call.
    pub ack_wait: Duration,
    /// Messages handled at once.
    pub concurrency: usize,
    /// Time between recovery passes.
    pub sweep_every: Duration,
    pub policy: RecoveryPolicy,
    /// Who the recovery pass signs its interruptions and cancellations as.
    pub by: Agent,
}

#[cfg(feature = "native")]
impl Default for RunConfig {
    fn default() -> Self {
        RunConfig {
            consumer: "comp-resume".into(),
            ack_wait: Duration::from_secs(60),
            concurrency: 8,
            sweep_every: Duration::from_secs(15),
            policy: RecoveryPolicy::default(),
            by: Agent {
                id: "comp-resume".into(),
                goal: Some("recovery sweep".into()),
                model: None,
            },
        }
    }
}

/// Run a resumer until `shutdown` flips to `true`: deliver every message on
/// `PARK_WAKE` to `sink` and sweep on a timer, concurrently.
///
/// Returns once in-flight messages have finished. Dying instead of returning is
/// the case this is built for: whatever was in flight is redelivered by the
/// server after `ack_wait`, and the sink sees the same [`Delivery`] again.
#[cfg(feature = "native")]
pub async fn run<S, K>(
    engine: std::sync::Arc<Engine<S>>,
    wake: std::sync::Arc<crate::nats::WakeStream>,
    sink: std::sync::Arc<K>,
    cfg: RunConfig,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()>
where
    S: ParkStore + 'static,
    K: Sink + 'static,
{
    use std::sync::Arc;

    use tokio::sync::Semaphore;

    use crate::engine::now_ms;

    let concurrency = cfg.concurrency.max(1);
    let mut consumer = wake.consumer(&cfg.consumer, cfg.ack_wait).await?;

    let sweeper = {
        let (engine, wake, cfg, mut shutdown) =
            (engine.clone(), wake.clone(), cfg.clone(), shutdown.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(cfg.sweep_every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = shutdown.changed() => break,
                    _ = tick.tick() => {}
                }
                match sweep(&engine, &*wake, &cfg.by, &cfg.policy, now_ms()).await {
                    Ok(s) => {
                        if !s.repaired.is_empty() {
                            eprintln!("holon-park resume: repaired indexes: {:?}", s.repaired);
                        }
                        for r in &s.recovered {
                            eprintln!(
                                "holon-park resume: {:?} {} (session {})",
                                r.action, r.ticket, r.session
                            );
                        }
                        for (ticket, e) in &s.unannounced {
                            eprintln!("holon-park resume: could not announce {ticket}: {e}");
                        }
                    }
                    Err(e) => eprintln!("holon-park resume: recovery pass failed: {e}"),
                }
            }
        })
    };

    let permits = Arc::new(Semaphore::new(concurrency));
    let mut shutdown = shutdown;
    let mut ended: Result<()> = Ok(());
    loop {
        let permit = tokio::select! {
            _ = shutdown.changed() => break,
            p = permits.clone().acquire_owned() => match p {
                Ok(p) => p,
                Err(_) => break,
            },
        };
        let next = tokio::select! {
            _ = shutdown.changed() => break,
            n = consumer.next() => n,
        };
        let delivery = match next {
            Ok(Some(d)) => d,
            Ok(None) => {
                ended = Err(ParkError::storage("the PARK_WAKE consumer's stream ended"));
                break;
            }
            Err(e) => {
                // Transient: the client re-pulls by itself; only slow the retry.
                eprintln!("holon-park resume: reading PARK_WAKE: {e}");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        let (engine, sink, by) = (engine.clone(), sink.clone(), cfg.by.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let delay = backoff(delivery.delivered);
            let verdict = handle(&engine, &*sink, &delivery.wake, &by, now_ms()).await;
            let answered = match verdict {
                Ok(h) if h.is_finished() => delivery.ack().await,
                Ok(h) => {
                    if let Handled::Retry(why) = &h {
                        eprintln!(
                            "holon-park resume: {} not delivered ({why}); again in {delay:?}",
                            delivery.wake.ticket
                        );
                    }
                    delivery.nak(delay).await
                }
                Err(e) => {
                    eprintln!(
                        "holon-park resume: {}: {e}; again in {delay:?}",
                        delivery.wake.ticket
                    );
                    delivery.nak(delay).await
                }
            };
            if let Err(e) = answered {
                // The server redelivers after `ack_wait`; the sink dedupes.
                eprintln!("holon-park resume: answering PARK_WAKE: {e}");
            }
        });
    }

    // In-flight messages finish before this returns, and so does a pass that
    // was under way (a pass cut short only costs a re-announcement).
    let _ = permits.acquire_many(concurrency as u32).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), sweeper).await;
    ended
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_a_second_and_stops_at_five_minutes() {
        let secs: Vec<u64> = (0..=12).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, [1, 1, 2, 4, 8, 16, 32, 64, 128, 256, 256, 256, 256]);
        assert_eq!(backoff(u64::MAX).as_secs(), 256);
    }

    #[test]
    fn a_delivery_is_the_documented_json() {
        let d = Delivery {
            ticket: "t1".into(),
            session: "s1".into(),
            correlation: "c1".into(),
            event: Event::Redispatch { attempt: 2, description: "d".into() },
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "ticket": "t1", "session": "s1", "correlation": "c1",
                "event": "redispatch", "attempt": 2, "description": "d"
            })
        );
        assert_eq!(serde_json::from_value::<Delivery>(v).unwrap(), d);
    }

    #[test]
    fn what_a_delivery_says_is_its_own_identity() {
        let key = |event| {
            Delivery { ticket: "t".into(), session: "s".into(), correlation: "c".into(), event }
                .idempotency_key()
        };
        let answered =
            Event::Answered { answer: CallResult { ok: true, body: vec![], detail: None } };
        assert_eq!(key(answered.clone()), "t:answered");
        assert_eq!(
            key(answered),
            key(Event::Answered {
                answer: CallResult { ok: false, body: vec![1], detail: Some("x".into()) },
            })
        );
        assert_ne!(
            key(Event::Redispatch { attempt: 1, description: String::new() }),
            key(Event::Redispatch { attempt: 2, description: String::new() })
        );
    }
}
