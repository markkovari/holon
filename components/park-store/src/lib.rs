//! `park-store` — `holon:park/lot` with `holon-park`'s engine in-process (ADR-0132): tickets are
//! records in this app's keyvalue, moved by compare-and-set, and `sweep` (the host's pump) runs the
//! recovery pass and hands what it resolves to the sink. Failures are `storage-error`, which the
//! contract says a caller may retry.
//!
//! Config (wasi:config/store):
//!   park-sink-url         where resolutions are delivered, e.g. comp-agentd's /park/deliver
//!   park-sink-token       its bearer token, if it wants one
//!   park-redeliver-secs   how long a resolution may sit untaken before it is delivered again, default 1
//!   park-deadline-grace-secs  how long past its deadline a parked call sits before the pass acts, default 10

#[allow(warnings)]
mod bindings;
mod kvstore;
mod witconv;

use bindings::exports::holon::park::lot::Guest as Lot;
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};

use holon_park::model as m;
use holon_park::recover::RecoveryPolicy;
use holon_park::resume::{self, Delivery, Handled, Sink, SinkReply};
use holon_park::{serve, wire, Engine};
use kvstore::{block_on, kv_store, KvStore};
use serde::de::DeserializeOwned;
use serde::Serialize;
use witconv::{ToModel, ToWit};

mod wit {
    pub use crate::bindings::holon::park::types as t;
}
use wit::t;

struct Component;

/// A sink that hangs is a delivery to try again on the next sweep, not a stuck one.
const SINK_TIMEOUT_NS: u64 = 30_000_000_000;

fn storage(msg: impl Into<String>) -> t::ParkError {
    t::ParkError::StorageError(msg.into())
}

fn cfg(key: &str) -> Option<String> {
    config::get(key).ok().flatten().filter(|s| !s.is_empty())
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), t::ParkError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(storage(format!("park-sink-url must be http(s), got {url:?}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].trim_end_matches('/').to_string()),
        None => (rest.to_string(), String::new()),
    };
    Ok((scheme, authority, path))
}

/// One WIT function, by name, over the wire `comp-park` spoke, answered by the engine here.
/// What it announces is delivered by `sweep`, not now: the sink may be the very caller (comp-agentd
/// waking its own approval), and a delivery inside its call would be answered by itself.
fn post(func: &str, body: &[u8]) -> Result<(u16, Vec<u8>), t::ParkError> {
    let engine = Engine::new(kv_store());
    let a = block_on(serve::dispatch(&engine, func, body, holon_park::engine::now_ms()));
    Ok((a.status, serde_json::to_vec(&a.body).unwrap_or_default()))
}

/// `POST park-sink-url`, the delivery as JSON under its idempotency key (ADR-0106's sink).
struct HttpSink {
    url: String,
}

impl HttpSink {
    fn send(&self, d: &Delivery) -> Result<u16, String> {
        let (scheme, authority, path) = parse_url(&self.url).map_err(|e| format!("{e:?}"))?;
        let headers = Fields::new();
        let set = |k: &str, v: &str| {
            let _ = headers.set(k, &[v.as_bytes().to_vec()]);
        };
        set("content-type", "application/json");
        set("idempotency-key", &d.idempotency_key());
        if let Some(token) = cfg("park-sink-token") {
            set("authorization", &format!("Bearer {token}"));
        }
        let req = OutgoingRequest::new(headers);
        req.set_method(&Method::Post).map_err(|_| "method")?;
        req.set_scheme(Some(&scheme)).map_err(|_| "scheme")?;
        req.set_authority(Some(&authority)).map_err(|_| "authority")?;
        req.set_path_with_query(Some(if path.is_empty() { "/" } else { &path }))
            .map_err(|_| "path")?;
        let body = serde_json::to_vec(d).map_err(|e| e.to_string())?;
        let out = req.body().map_err(|_| "body")?;
        {
            let stream = out.write().map_err(|_| "write")?;
            // Chunked: `blocking-write-and-flush` traps above 4096 bytes.
            for chunk in body.chunks(4096) {
                stream.blocking_write_and_flush(chunk).map_err(|e| format!("{e:?}"))?;
            }
        }
        OutgoingBody::finish(out, None).map_err(|_| "finish")?;
        let opts = RequestOptions::new();
        let _ = opts.set_connect_timeout(Some(SINK_TIMEOUT_NS));
        let _ = opts.set_first_byte_timeout(Some(SINK_TIMEOUT_NS));
        let fut = outgoing_handler::handle(req, Some(opts)).map_err(|e| format!("{e:?}"))?;
        fut.subscribe().block();
        let resp = fut
            .get()
            .ok_or("no response")?
            .map_err(|_| "response taken")?
            .map_err(|e| format!("{e:?}"))?;
        // The status is the answer; the body is not read.
        Ok(resp.status())
    }
}

impl Sink for HttpSink {
    async fn deliver(&self, d: &Delivery) -> SinkReply {
        match self.send(d) {
            Ok(s) if (200..300).contains(&s) => SinkReply::Delivered,
            Ok(410) => SinkReply::Gone("the sink answered 410".into()),
            Ok(s) => SinkReply::Retry(format!("the sink answered {s}")),
            Err(e) => SinkReply::Retry(e),
        }
    }
}

/// What the recovery pass announces is handed straight to the sink.
struct Deliver<'a> {
    engine: &'a Engine<KvStore>,
    sink: &'a HttpSink,
    delivered: std::sync::atomic::AtomicU32,
}

impl resume::Announcer for Deliver<'_> {
    async fn announce(&self, msg: &m::WakeMessage) -> holon_park::error::Result<()> {
        let by = resumer();
        match resume::handle(self.engine, self.sink, msg, &by, holon_park::engine::now_ms()).await?
        {
            Handled::Delivered | Handled::Gone => {
                self.delivered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            // Left as it is: the next sweep delivers it again.
            Handled::Retry(_) | Handled::Stale => {}
        }
        Ok(())
    }
}

fn resumer() -> m::Agent {
    m::Agent { id: "park-store".into(), goal: None, model: None }
}

fn sweep() -> Result<u32, t::ParkError> {
    let url = cfg("park-sink-url")
        .ok_or_else(|| storage("park-sink-url is not set: nowhere to deliver"))?;
    let engine = Engine::new(kv_store());
    let sink = HttpSink { url };
    let redeliver = cfg("park-redeliver-secs").and_then(|s| s.parse().ok()).unwrap_or(1u64);
    let grace = cfg("park-deadline-grace-secs").and_then(|s| s.parse::<u64>().ok());
    let mut policy = RecoveryPolicy { reannounce_after_ms: redeliver * 1000, ..Default::default() };
    if let Some(g) = grace {
        policy.deadline_grace_ms = g * 1000;
    }
    let deliver = Deliver { engine: &engine, sink: &sink, delivered: Default::default() };
    let now = holon_park::engine::now_ms();
    block_on(resume::sweep(&engine, &deliver, &resumer(), &policy, now)).map_err(|e| e.to_wit())?;
    Ok(deliver.delivered.into_inner())
}

fn decode<T: DeserializeOwned>(status: u16, body: &[u8]) -> Result<T, t::ParkError> {
    if status == 200 {
        return serde_json::from_slice(body).map_err(|e| {
            storage(format!("the park engine answered a body this cannot read: {e}"))
        });
    }
    match serde_json::from_slice::<wire::ErrorBody>(body) {
        Ok(e) => Err(e.into_error().to_wit()),
        Err(_) => {
            let text = String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned();
            Err(storage(format!("the park engine answered {status}: {text}")))
        }
    }
}

fn call<Q: Serialize, R: DeserializeOwned + ToWit>(
    func: &str,
    req: &Q,
) -> Result<R::Out, t::ParkError> {
    let body = serde_json::to_vec(req).map_err(|e| t::ParkError::Invalid(e.to_string()))?;
    let (status, raw) = post(func, &body)?;
    decode::<R>(status, &raw).map(ToWit::to_wit)
}

impl Lot for Component {
    fn park(
        session: String,
        call_: t::OutboundCall,
        by: t::Agent,
    ) -> Result<t::ParkResult, t::ParkError> {
        let req = wire::ParkRequest { session, call: call_.to_model(), by: by.to_model() };
        call::<_, m::ParkResult>("park", &req)
    }

    fn wake(correlation: String, answer: t::CallResult) -> Result<String, t::ParkError> {
        let req = wire::WakeRequest { correlation, answer: answer.to_model() };
        call::<_, String>("wake", &req)
    }

    fn pending(session: String) -> Result<Vec<t::TicketEntry>, t::ParkError> {
        call::<_, Vec<m::TicketEntry>>("pending", &wire::Session { session })
    }

    fn take_ready(ticket: String) -> Result<t::Resolution, t::ParkError> {
        call::<_, m::Resolution>("take-ready", &wire::Ticket { ticket })
    }

    fn commit(
        session: String,
        effects: Vec<t::Effect>,
        by: t::Agent,
    ) -> Result<t::CommitResult, t::ParkError> {
        let req = wire::CommitRequest {
            session,
            effects: effects.into_iter().map(ToModel::to_model).collect(),
            by: by.to_model(),
        };
        call::<_, m::CommitResult>("commit", &req)
    }

    fn interrupt(
        ticket: String,
        reason: String,
        by: t::Agent,
    ) -> Result<t::TurnStatus, t::ParkError> {
        let req = wire::InterruptRequest { ticket, reason, by: by.to_model() };
        call::<_, m::TurnStatus>("interrupt", &req)
    }

    fn cancel(ticket: String, by: t::Agent) -> Result<(), t::ParkError> {
        call::<_, ()>("cancel", &wire::CancelRequest { ticket, by: by.to_model() })
    }

    fn oplog(
        session: String,
        after: Option<u64>,
        limit: u32,
    ) -> Result<Vec<t::TicketEntry>, t::ParkError> {
        call::<_, Vec<m::TicketEntry>>("oplog", &wire::OplogRequest { session, after, limit })
    }

    fn sweep() -> Result<u32, t::ParkError> {
        sweep()
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;
    use holon_park::ParkError;

    fn body(e: &ParkError) -> Vec<u8> {
        serde_json::to_vec(&wire::ErrorBody::from(e)).unwrap()
    }

    #[test]
    fn engine_refusals_map_back_to_their_park_error_case() {
        let got = decode::<m::ParkResult>(404, &body(&ParkError::NotFound("t1".into())));
        assert!(matches!(got, Err(t::ParkError::NotFound(s)) if s == "t1"));
        let got = decode::<m::Resolution>(409, &body(&ParkError::AlreadyClosed("t1".into())));
        assert!(matches!(got, Err(t::ParkError::AlreadyClosed(s)) if s == "t1"));
        let got = decode::<m::ParkResult>(400, &body(&ParkError::Invalid("bad".into())));
        assert!(matches!(got, Err(t::ParkError::Invalid(s)) if s == "bad"));
        let got = decode::<m::ParkResult>(503, &body(&ParkError::Storage("down".into())));
        assert!(matches!(got, Err(t::ParkError::StorageError(s)) if s == "down"));
    }

    #[test]
    fn what_is_not_the_engines_json_is_a_storage_error() {
        assert!(
            matches!(decode::<String>(502, b"<html>bad gateway</html>"), Err(t::ParkError::StorageError(s)) if s.contains("502"))
        );
        assert!(matches!(decode::<String>(200, b"not json"), Err(t::ParkError::StorageError(_))));
        assert_eq!(decode::<String>(200, b"\"tkt\"").ok().as_deref(), Some("tkt"));
    }

    #[test]
    fn a_url_with_a_base_path_keeps_it() {
        let (_, auth, base) = parse_url("http://127.0.0.1:8015/").unwrap();
        assert_eq!((auth.as_str(), base.as_str()), ("127.0.0.1:8015", ""));
        let (_, _, base) = parse_url("https://park.internal/api").unwrap();
        assert_eq!(base, "/api");
        assert!(parse_url("nats://x").is_err());
    }

    #[test]
    fn records_survive_model_to_wit_and_back() {
        let call = m::OutboundCall {
            correlation: "c1".into(),
            description: "d".into(),
            deadline: Some(123),
            poll: Some(m::PollSpec { url: "http://x".into(), interval_secs: 5 }),
            replay: m::ReplayPolicy::Safe,
            parent: Some("g".into()),
            group: None,
        };
        assert_eq!(call.clone().to_wit().to_model(), call);
        let group = m::OutboundCall {
            parent: None,
            group: Some(m::GroupSpec { policy: m::JoinPolicy::FailFast, children: 3 }),
            ..call
        };
        assert_eq!(group.clone().to_wit().to_model(), group);
        let entry = m::TicketEntry {
            ticket: "t".into(),
            session: "s".into(),
            status: m::TurnStatus::Interrupted,
            parked_at: 1,
            woken_at: None,
            resumed_at: None,
            correlation: "c1".into(),
            replay: m::ReplayPolicy::Abandon,
            deadline: Some(9),
            redispatches: 2,
            interrupted_at: Some(5),
            parent: Some("gt".into()),
            group: Some(m::GroupSpec { policy: m::JoinPolicy::AllSettled, children: 2 }),
        };
        assert_eq!(entry.clone().to_wit().to_model(), entry);
    }

    #[test]
    fn a_commits_effects_survive_model_to_wit_and_back() {
        let effects = vec![
            m::Effect::Park(m::OutboundCall {
                correlation: "c".into(),
                description: "d".into(),
                deadline: None,
                poll: None,
                replay: m::ReplayPolicy::Unsafe,
                parent: None,
                group: None,
            }),
            m::Effect::Wake(m::WakeEffect {
                correlation: "w".into(),
                answer: m::CallResult { ok: true, body: vec![1], detail: None },
            }),
            m::Effect::Interrupt(m::InterruptEffect {
                correlation: "i".into(),
                reason: "r".into(),
            }),
            m::Effect::Cancel("x".into()),
        ];
        for e in effects {
            assert_eq!(e.clone().to_wit().to_model(), e);
        }
        let result = m::CommitResult {
            batch: "b".into(),
            parked: vec![m::ParkResult { ticket: "t".into(), outcome: m::ParkOutcome::Parked }],
        };
        assert_eq!(result.clone().to_wit().to_model(), result);
    }

    #[test]
    fn a_joined_resolution_survives_model_to_wit_and_back() {
        let by = m::Agent::named("join");
        let joined = m::Resolution::Joined(m::JoinResult {
            outcome: m::JoinOutcome::FailedFast,
            missing: 1,
            children: vec![
                m::ChildOutcome {
                    ticket: "a".into(),
                    correlation: "ca".into(),
                    ended: m::ChildResult::Answered(m::CallResult {
                        ok: false,
                        body: vec![1],
                        detail: Some("no".into()),
                    }),
                },
                m::ChildOutcome {
                    ticket: "b".into(),
                    correlation: "cb".into(),
                    ended: m::ChildResult::Interrupted(m::Interruption {
                        reason: "r".into(),
                        at: 3,
                        by,
                    }),
                },
                m::ChildOutcome {
                    ticket: "c".into(),
                    correlation: "cc".into(),
                    ended: m::ChildResult::Cancelled,
                },
            ],
        });
        assert_eq!(joined.clone().to_wit().to_model(), joined);
        for o in [
            m::JoinOutcome::AllAnswered,
            m::JoinOutcome::SomeFailed,
            m::JoinOutcome::FailedFast,
            m::JoinOutcome::TimedOut,
        ] {
            assert_eq!(o.to_wit().to_model(), o);
        }
    }

    #[test]
    fn a_resolution_survives_model_to_wit_and_back() {
        let answered = m::Resolution::Answered(m::CallResult {
            ok: false,
            body: vec![1, 2],
            detail: Some("no".into()),
        });
        assert_eq!(answered.clone().to_wit().to_model(), answered);
        let interrupted = m::Resolution::Interrupted(m::Interruption {
            reason: "r".into(),
            at: 7,
            by: m::Agent::named("sweeper"),
        });
        assert_eq!(interrupted.clone().to_wit().to_model(), interrupted);
        for p in [m::ReplayPolicy::Safe, m::ReplayPolicy::Unsafe, m::ReplayPolicy::Abandon] {
            assert_eq!(p.to_wit().to_model(), p);
        }
    }
}
