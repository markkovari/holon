//! One WIT function, by name, over the JSON [`crate::wire`] defines: what `comp-park` served over
//! HTTP, and what `park-store` now runs in-process (ADR-0132).

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::engine::Engine;
use crate::error::ParkError;
use crate::model::{Resolution, WakeMessage};
use crate::store::ParkStore;
use crate::wire;

/// A status as `comp-park` gave it, the JSON body, and what the call says to announce: the
/// ticket it woke, or the group it resolved (ADR-0109), never a child on its own.
pub struct Answer {
    pub status: u16,
    pub body: Value,
    pub announce: Vec<WakeMessage>,
}

fn json_of<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn refusal(e: &ParkError) -> Answer {
    let body = wire::ErrorBody::from(e);
    Answer { status: wire::status_of(&body.error), body: json_of(&body), announce: Vec::new() }
}

fn answer<T: Serialize>(r: Result<T, ParkError>) -> Answer {
    match r {
        Ok(v) => Answer { status: 200, body: json_of(&v), announce: Vec::new() },
        Err(e) => refusal(&e),
    }
}

fn announcing<T: Serialize>(r: Result<(T, Vec<WakeMessage>), ParkError>) -> Answer {
    match r {
        Ok((v, announce)) => Answer { status: 200, body: json_of(&v), announce },
        Err(e) => refusal(&e),
    }
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, Answer> {
    serde_json::from_slice(body).map_err(|e| Answer {
        status: 400,
        body: json_of(&wire::ErrorBody::new("bad-request", e.to_string())),
        announce: Vec::new(),
    })
}

pub async fn dispatch<S: ParkStore>(
    engine: &Engine<S>,
    func: &str,
    body: &[u8],
    now_ms: u64,
) -> Answer {
    match dispatch_inner(engine, func, body, now_ms).await {
        Ok(a) | Err(a) => a,
    }
}

async fn dispatch_inner<S: ParkStore>(
    e: &Engine<S>,
    func: &str,
    body: &[u8],
    now_ms: u64,
) -> Result<Answer, Answer> {
    Ok(match func {
        "park" => {
            let req: wire::ParkRequest = parse(body)?;
            answer(e.park(&req.session, req.call, req.by, now_ms).await)
        }
        "wake" => {
            let req: wire::WakeRequest = parse(body)?;
            announcing(
                e.wake(&req.correlation, req.answer, now_ms).await.map(|o| (o.ticket, o.announce)),
            )
        }
        "pending" => {
            let req: wire::Session = parse(body)?;
            answer(e.pending(&req.session, now_ms).await)
        }
        "take-ready" => {
            let req: wire::Ticket = parse(body)?;
            answer::<Resolution>(e.take_ready(&req.ticket, now_ms).await)
        }
        "interrupt" => {
            let req: wire::InterruptRequest = parse(body)?;
            let r = e.interrupt(&req.ticket, &req.reason, req.by, now_ms).await;
            announcing(r.map(|o| (o.status, o.announce)))
        }
        "cancel" => {
            let req: wire::CancelRequest = parse(body)?;
            // Cancelling a child can resolve its group, which is then what is announced.
            announcing(e.cancel_announcing(&req.ticket, req.by, now_ms).await.map(|a| ((), a)))
        }
        "commit" => {
            let req: wire::CommitRequest = parse(body)?;
            let r = e.commit(&req.session, req.effects, req.by, now_ms).await;
            announcing(r.map(|c| (c.result, c.announce)))
        }
        "oplog" => {
            let req: wire::OplogRequest = parse(body)?;
            answer(e.oplog(&req.session, req.after, req.limit, now_ms).await)
        }
        other => Answer {
            status: 404,
            body: json_of(&wire::ErrorBody::new("not-found", format!("no such route: {other}"))),
            announce: Vec::new(),
        },
    })
}
