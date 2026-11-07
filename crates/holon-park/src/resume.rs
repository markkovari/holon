//! A resumer (ADR-0106): deliver at least once, then close; recover on a timer.

use std::future::Future;

use serde::{Deserialize, Serialize};

use crate::batch::BatchRecovered;
use crate::engine::Engine;
use crate::error::{ParkError, Result};
use crate::model::{
    Agent, CallResult, Interruption, JoinResult, Resolution, SessionId, TicketId, WakeKind,
    WakeMessage,
};
use crate::recover::{Recovered, RecoveryPolicy, Repaired};
use crate::store::{ParkStore, StoredStatus};

/// `comp-resume`'s HTTP sink receives this as one JSON object:
///
/// ```json
/// {"ticket": "…", "session": "…", "correlation": "…", "event": "answered",
///  "answer": {"ok": true, "body": [42], "detail": null}}
/// {"ticket": "…", "session": "…", "correlation": "…", "event": "interrupted",
///  "interruption": {"reason": "…", "at": 1700000000000, "by": {"id": "comp-resume"}}}
/// {"ticket": "…", "session": "…", "correlation": "…", "event": "redispatch",
///  "attempt": 2, "description": "…"}
/// {"ticket": "…", "session": "…", "correlation": "…", "event": "joined",
///  "joined": {"outcome": "all-answered", "missing": 0, "children": [
///    {"ticket": "…", "correlation": "…", "ended": {"answered": {"ok": true, "body": [], "detail": null}}}]}}
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Delivery {
    pub ticket: TicketId,
    pub session: SessionId,
    /// How the turn finds the call in its own journal.
    pub correlation: String,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    /// `ok: false` is still an answer.
    Answered { answer: CallResult },
    /// The call may or may not have happened: tell the turn, do not repeat the call.
    Interrupted { interruption: Interruption },
    /// The turn owns the payload; this crate only ever held a description of it.
    Redispatch { attempt: u32, description: String },
    /// The children are not delivered on their own (ADR-0109).
    Joined { joined: JoinResult },
}

impl Delivery {
    /// Stable across redeliveries; each re-dispatch is its own.
    pub fn idempotency_key(&self) -> String {
        match &self.event {
            Event::Answered { .. } => format!("{}:answered", self.ticket),
            Event::Interrupted { .. } => format!("{}:interrupted", self.ticket),
            Event::Redispatch { attempt, .. } => format!("{}:redispatch:{attempt}", self.ticket),
            Event::Joined { .. } => format!("{}:joined", self.ticket),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkReply {
    /// Accepted now, or earlier (a redelivery it already had).
    Delivered,
    /// The session is gone or closed: retrying would never end, so the ticket is cancelled.
    Gone(String),
    Retry(String),
}

pub trait Sink: Send + Sync {
    fn deliver(&self, delivery: &Delivery) -> impl Future<Output = SinkReply> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handled {
    Delivered,
    /// The ticket is gone, taken or cancelled, or a later re-dispatch superseded this one.
    Stale,
    Gone,
    /// Not delivered: the next sweep tries again.
    Retry(String),
}

impl Handled {
    pub fn is_finished(&self) -> bool {
        !matches!(self, Handled::Retry(_))
    }
}

/// A message is a nudge, not the truth: what is done is decided by the ticket as it is now.
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
        // A child is consumed by its group, so a message that names one is a leftover.
        (WakeKind::Resolved, StoredStatus::Ready | StoredStatus::Interrupted)
            if record.call.parent.is_some() =>
        {
            return Ok(Handled::Stale)
        }
        (WakeKind::Resolved, StoredStatus::Ready | StoredStatus::Interrupted) => {
            match engine.resolution(&msg.ticket).await? {
                Resolution::Answered(answer) => Event::Answered { answer },
                Resolution::Interrupted(interruption) => Event::Interrupted { interruption },
                Resolution::Joined(joined) => Event::Joined { joined },
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

pub trait Announcer: Send + Sync {
    fn announce(&self, msg: &WakeMessage) -> impl Future<Output = Result<()>> + Send;
}

#[derive(Debug, Default)]
pub struct Swept {
    pub repaired: Repaired,
    pub batches: Vec<BatchRecovered>,
    pub recovered: Vec<Recovered>,
    /// The next pass re-announces a resolved one; a lost `redispatch` costs one bounded attempt.
    pub unannounced: Vec<(TicketId, ParkError)>,
}

/// Repair first, so the tickets acted on are ones `wake` and `pending` can see; commits before
/// recovery, so the tickets they park are there for it.
pub async fn sweep<S: ParkStore, A: Announcer>(
    engine: &Engine<S>,
    announcer: &A,
    by: &Agent,
    policy: &RecoveryPolicy,
    now_ms: u64,
) -> Result<Swept> {
    let repaired = engine.repair().await?;
    let batches = engine.complete_batches(policy.batch_lease_ms, now_ms).await?;
    let recovered = engine.recover(by, policy, now_ms).await?;
    let mut unannounced = Vec::new();
    for b in &batches {
        for msg in &b.announce {
            if let Err(e) = announcer.announce(msg).await {
                unannounced.push((msg.ticket.clone(), e));
            }
        }
    }
    for r in &recovered {
        if let Some(msg) = r.announcement() {
            if let Err(e) = announcer.announce(&msg).await {
                unannounced.push((r.ticket.clone(), e));
            }
        }
    }
    Ok(Swept { repaired, batches, recovered, unannounced })
}

#[cfg(test)]
mod tests {
    use super::*;

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
