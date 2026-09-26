//! The JSON `comp-park` speaks over HTTP — one definition, the same shape as
//! `holon-vcs::wire` (ADR-0099's *The service*, applied here per ADR-0100).
//!
//! Every route is `POST /v1/<the WIT function's name>` with one JSON object as
//! the body, and answers with the function's `ok` value (`200`) or an
//! [`ErrorBody`] (see [`status_of`]). The model types already serialise
//! field-for-field like the WIT (kebab-case); the envelopes below are for
//! functions that take more than one argument.

use serde::{Deserialize, Serialize};

use crate::error::ParkError;
use crate::model::{Agent, CallResult, OutboundCall, SessionId, TicketId};

pub const ROUTES: &[&str] = &["park", "wake", "pending", "take-ready", "cancel", "oplog"];

pub fn route(func: &str) -> String {
    format!("/v1/{func}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ParkRequest {
    pub session: SessionId,
    pub call: OutboundCall,
    pub by: Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WakeRequest {
    pub correlation: String,
    pub answer: CallResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Session {
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Ticket {
    pub ticket: TicketId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CancelRequest {
    pub ticket: TicketId,
    pub by: Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OplogRequest {
    pub session: SessionId,
    #[serde(default)]
    pub after: Option<u64>,
    pub limit: u32,
}

/// A refusal on the wire: `error` is the [`ParkError`] case, kebab-case, and
/// `detail` its payload as JSON (a string for every case here — `park-error`
/// has no structured payload the way `vcs-error`'s `concurrent-modification`
/// does). `message` is for people and never parsed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ErrorBody {
    pub error: String,
    #[serde(default)]
    pub detail: serde_json::Value,
    #[serde(default)]
    pub message: String,
}

impl ErrorBody {
    pub fn new(error: &str, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        ErrorBody {
            error: error.to_string(),
            message: format!("{error}: {detail}"),
            detail: serde_json::Value::String(detail),
        }
    }

    /// Back to the contract's error. Anything this side does not recognise is
    /// `storage-error` naming it: the caller learns the store refused, and
    /// retrying is the only thing it could do about an answer it cannot read.
    pub fn into_error(self) -> ParkError {
        let text = |v: &serde_json::Value| match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        };
        match self.error.as_str() {
            "not-found" => ParkError::NotFound(text(&self.detail)),
            "already-closed" => ParkError::AlreadyClosed(text(&self.detail)),
            "invalid" => ParkError::Invalid(text(&self.detail)),
            "bad-request" => ParkError::Invalid(format!("bad-request: {}", text(&self.detail))),
            "storage-error" => ParkError::Storage(text(&self.detail)),
            _ => ParkError::Storage(format!("comp-park answered {}: {}", self.error, self.message)),
        }
    }
}

impl From<&ParkError> for ErrorBody {
    fn from(e: &ParkError) -> Self {
        let (error, detail) = match e {
            ParkError::NotFound(s) => ("not-found", serde_json::Value::String(s.clone())),
            ParkError::AlreadyClosed(s) => ("already-closed", serde_json::Value::String(s.clone())),
            ParkError::Storage(s) => ("storage-error", serde_json::Value::String(s.clone())),
            ParkError::Invalid(s) => ("invalid", serde_json::Value::String(s.clone())),
        };
        ErrorBody { error: error.to_string(), detail, message: e.to_string() }
    }
}

pub fn status_of(error: &str) -> u16 {
    match error {
        "invalid" | "bad-request" => 400,
        "not-found" => 404,
        // A ticket that is `resumed`/`cancelled` already: the caller's own
        // state is stale, not a request to retry as-is.
        "already-closed" => 409,
        // storage-error and anything unknown: the store, not the request.
        _ => 503,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_park_error_round_trips_through_its_wire_form() {
        let cases = [
            ParkError::NotFound("t1".into()),
            ParkError::AlreadyClosed("t1".into()),
            ParkError::Storage("nats is down".into()),
            ParkError::Invalid("bad correlation".into()),
        ];
        for e in cases {
            let body = ErrorBody::from(&e);
            assert_eq!(body.into_error(), e, "{e:?} must round-trip");
        }
    }

    #[test]
    fn status_of_covers_every_case_this_crate_emits() {
        for (error, want) in [
            ("not-found", 404),
            ("already-closed", 409),
            ("invalid", 400),
            ("bad-request", 400),
            ("storage-error", 503),
            ("something-unrecognised", 503),
        ] {
            assert_eq!(status_of(error), want, "{error}");
        }
    }
}
