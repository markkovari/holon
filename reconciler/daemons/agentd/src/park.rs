//! `comp-park` as `comp-agentd` uses it (ADR-0113): one ticket for each approval a human has
//! not yet given, so that the wait survives this process, woken by the human's answer.
//!
//! Loopback HTTP and one JSON object a call, the wire `park-store` speaks, with `holon-park`'s own
//! request and error types: nothing here is redefined, so a change to the contract is a compile
//! error and not a runtime surprise.

use std::time::Duration;

use holon_park::engine::now_ms;
use holon_park::error::ParkError;
use holon_park::model::{Agent, CallResult, OutboundCall, ParkResult, ReplayPolicy};
use holon_park::wire::{CancelRequest, ErrorBody, ParkRequest, WakeRequest};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::wire::{ApprovalScope, Decision};

pub struct Park {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
    /// The waits between tries when a park or a wake does not get through: a few, short, because
    /// a human or a task is waiting on the other end of each.
    retry: Vec<Duration>,
}

/// A human's decision as it rides inside a ticket: the body of the answer `wake` is given, and what
/// the sink reads back out of the delivery.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Answered {
    pub decision: Decision,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub decided_by: String,
    /// How far an approval reaches (ADR-0114). A ticket answered by a daemon that did not know it
    /// has none: `once`, which is the safe reading.
    #[serde(default)]
    pub scope: ApprovalScope,
}

impl Park {
    pub fn new(http: reqwest::Client, base: String, token: Option<String>) -> Park {
        Park { http, base, token, retry: vec![Duration::from_secs(1), Duration::from_secs(2)] }
    }

    /// How long to wait between tries (the tests make it short).
    #[cfg(test)]
    pub fn with_retry(mut self, retry: Vec<Duration>) -> Park {
        self.retry = retry;
        self
    }

    async fn call<B: Serialize, T: DeserializeOwned>(
        &self,
        func: &str,
        body: &B,
    ) -> Result<T, ParkError> {
        let mut req = self.http.post(format!("{}/v1/{func}", self.base.trim_end_matches('/')));
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let r = req.json(body).send().await.map_err(|e| ParkError::storage(format!("{e:#}")))?;
        let status = r.status();
        let bytes = r.bytes().await.map_err(ParkError::storage)?;
        if status.is_success() {
            return serde_json::from_slice(&bytes).map_err(ParkError::storage);
        }
        match serde_json::from_slice::<ErrorBody>(&bytes) {
            Ok(body) => Err(body.into_error()),
            Err(_) => Err(ParkError::storage(format!("comp-park answered {status}"))),
        }
    }

    /// Park the approval of `call_id` in `session`: the ticket, new or already there. `deadline` is
    /// when to give up and treat the silence as a denial; without one it waits as long as it takes.
    ///
    /// `unsafe` replay, which is what an approval is: past its deadline it is *interrupted*, and
    /// asking the human again would be a second prompt about a call they may be looking at.
    pub async fn park(
        &self,
        session: &str,
        call_id: &str,
        what: &str,
        deadline: Option<u64>,
    ) -> Result<String, ParkError> {
        let req = ParkRequest {
            session: session.to_string(),
            call: OutboundCall {
                correlation: call_id.to_string(),
                description: what.to_string(),
                deadline,
                poll: None,
                replay: ReplayPolicy::Unsafe,
                parent: None,
                group: None,
            },
            by: Agent::named("comp-agentd"),
        };
        let r: ParkResult = self.call("park", &req).await?;
        Ok(r.ticket)
    }

    /// Park it, retrying a transient failure a few times. The task cannot wait without a ticket,
    /// so what is left when the tries are spent is the caller's to fail.
    pub async fn park_with_retries(
        &self,
        session: &str,
        call_id: &str,
        what: &str,
        deadline: Option<u64>,
    ) -> Result<String, ParkError> {
        let mut tries = self.retry.iter();
        loop {
            match self.park(session, call_id, what, deadline).await {
                Err(ParkError::Storage(e)) => match tries.next() {
                    Some(wait) => {
                        eprintln!("comp-agentd: parking {call_id}: {e}; trying again");
                        tokio::time::sleep(*wait).await;
                    }
                    None => return Err(ParkError::Storage(e)),
                },
                other => return other,
            }
        }
    }

    /// Give the ticket the human's answer. A ticket that is not there (a store that lost it, a
    /// crash between the checkpoint and the park) is made again and answered, so a human who
    /// answers is never told their answer has nowhere to go.
    pub async fn answer(
        &self,
        session: &str,
        call_id: &str,
        what: &str,
        deadline: Option<u64>,
        a: &Answered,
    ) -> Result<(), ParkError> {
        let wake = WakeRequest {
            correlation: call_id.to_string(),
            answer: CallResult {
                ok: true,
                body: serde_json::to_vec(a).unwrap_or_default(),
                detail: None,
            },
        };
        match self.call::<_, serde_json::Value>("wake", &wake).await {
            Ok(_) => Ok(()),
            Err(ParkError::NotFound(_)) => {
                self.park_with_retries(session, call_id, what, deadline).await?;
                self.call::<_, serde_json::Value>("wake", &wake).await.map(|_| ())
            }
            Err(e) => Err(e),
        }
    }

    /// Give up on a ticket whose task is over. Best effort: one that is missed is answered `410` by
    /// the sink when it is delivered, which cancels it from the other side.
    pub async fn cancel(&self, ticket: &str) -> Result<(), ParkError> {
        let req = CancelRequest { ticket: ticket.to_string(), by: Agent::named("comp-agentd") };
        match self.call::<_, serde_json::Value>("cancel", &req).await {
            Ok(_) | Err(ParkError::AlreadyClosed(_) | ParkError::NotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Unix milliseconds `after` from now.
    pub fn deadline_in(after: Duration) -> u64 {
        now_ms().saturating_add(after.as_millis() as u64)
    }
}
