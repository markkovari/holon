//! `comp-relay` — what turns a pull contract into a trigger.
//!
//! ## The gap this fills
//!
//! Three capabilities in this catalog describe work that happens *later*, and all
//! three are deliberately PULL:
//!
//! * `sched:timer@0.1.0`  — durable jobs with recurrence and a lease. Its own header
//!   says "the component owns the *when*; the relay owns the *what*".
//!   `saga-domain` drains it inside `pump()`.
//! * `event:bus@0.1.0`    — a durable log with per-consumer-group offsets.
//!   "Delivery is PULL (consumers poll) — there is no push/callback."
//! * `cron:expr@0.1.0`    — parses a cron string and computes fire times. Pure
//!   compute: it answers *when*, and fires nothing.
//!
//! Pull is the right choice — it keeps all three pure WASI, so they run on any host.
//! But it means something outside must poke the app, and until now nothing did. On
//! Kubernetes that something was a curl-loop Deployment (`bench/ESHOP-BENCH.md`);
//! on the native lane it was the operator refreshing a browser tab, which
//! `docs/apps/ESHOP.md` admits: the storefront page pumps because the native lane
//! "has no messaging plugin".
//!
//! This is that poke, as one small daemon.
//!
//! ## Why this is native, per ADR-0095
//!
//! 1. **Does it need something WASI does not give a guest?** Yes: a timer that keeps
//!    running when no request is in flight, and a held NATS subscription. A
//!    `wasm32-wasip2` component has no background — it exists between an incoming
//!    request and its response. That is the same reason `reconciler/` is native.
//! 2. **Is it the smallest it could be?** It decides only WHEN to poke and never
//!    what to do. Eligibility, recurrence and leasing stay inside `scheduler-timer`;
//!    offsets and at-least-once stay inside `event-bus`; the work stays in the app's
//!    own `pump`. This holds no business logic, and nothing it does is unavailable
//!    to a component for any reason other than the missing background.
//! 3. **Does it answer a contract a component could have answered?** It drives them
//!    rather than replacing them. `sched:timer` and `event:bus` stay in WIT,
//!    unchanged, and an app keeps calling them exactly as `saga-domain` already
//!    does. What this speaks is the app's own `wasi:http/incoming-handler` — the
//!    interface it already exports — so no app changes to gain a trigger.
//!
//! ## What it actually does
//!
//! `POST` to an endpoint, on a schedule. That is the whole daemon.
//!
//! `/internal/pump` is an established convention here — `saga-domain` and the four
//! `eshop-*` services all export it, and each drains its own timers and consumer
//! groups behind it. So the relay needs no WIT bindings and no knowledge of what a
//! job is: the app already knows, and a poke is all it was waiting for.
//!
//! ## An allow-list, for the reason `comp-fswatch` has one
//!
//! A target is a URL, and a URL from configuration is a request this process will
//! make on behalf of whoever wrote the config. `comp-checks` takes `--allow` because
//! "the input comes from an agent, and 'probably fine' is not a boundary"; a poker
//! aimed at an arbitrary host is a confused deputy with a timer attached.
//!
//! So every target is named on the command line. There is no wildcard and no
//! default: a relay started with no `--target` refuses to start rather than idling,
//! because a scheduler with nothing scheduled is nearly always a misconfiguration
//! that would otherwise be discovered as silence.
//!
//! ## Two paths, and why both
//!
//! **The sweep** is the correctness path: every target, every `--interval` seconds,
//! unconditionally. It is what drives time-based transitions that no event
//! announces — a grace period expiring, a retry becoming due — and it is what
//! catches up after this process was down.
//!
//! **The push** is the latency path, and only on a lattice. `event-bus` bumps a
//! dotted seq key per topic in NATS KV on every publish, so `$KV.<bucket>.eb.seq.>`
//! is a change feed of "somebody published something". Subscribing to it turns a
//! poll interval into milliseconds. `components/event-pusher` already does exactly
//! this on hosts with a `wasmcloud:messaging` plugin; this is the same idea for the
//! lane that has none.
//!
//! Push does NOT replace the sweep. Core-NATS delivery is at-most-once, so a relay
//! that is restarting drops the notifications published while it was gone — and no
//! KV change announces the passage of time. `event-pusher`'s own header says the
//! same thing about itself. Push is an optimisation; the sweep is the contract.
//!
//! ## One firing relay per lattice
//!
//! With `--nats-url`, a JetStream KV lease decides which relay acts, exactly as the
//! reconciler's does (`lattice/src/lease.rs`) — a key whose `max_age` IS the lease,
//! so a holder that stops renewing expires with nothing to clean up.
//!
//! It is needed for a sharper reason than the reconciler's. Two relays double-poking
//! a timer is merely wasteful: `timer.due` leases what it hands out, so the second
//! caller gets nothing. But `bus.poll` ADVANCES A CONSUMER GROUP'S OFFSET, and two
//! relays draining one group race over which of them sees an event — at-least-once
//! becomes at-most-once, silently. The lease is what stops that.
//!
//! Without `--nats-url` it fires alone and says so. That is right for tier 1, where
//! one box runs one app and there is no lattice to elect within.
//!
//!   comp-relay --target http://127.0.0.1:3012/internal/pump --interval 10
//!   comp-relay --target http://10.0.0.2:3401/internal/pump \
//!              --nats-url nats://10.0.0.1:4222 --lattice prod --watch-bus

use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use futures::StreamExt;

#[derive(Parser)]
#[command(
    name = "comp-relay",
    about = "Poke an app's pump endpoint on a schedule, so pull-based timers and topics fire."
)]
struct Args {
    /// An endpoint to POST, repeatable. Usually `<app>/internal/pump`.
    ///
    /// An allow-list, not a discovery mechanism: this process makes requests on
    /// behalf of whoever wrote the configuration, and a target it inferred is a
    /// request nobody authorised.
    #[arg(long = "target", required = true)]
    targets: Vec<String>,

    /// Seconds between sweeps. The completeness path — it must be short enough that
    /// a missed push is a delay rather than a stall.
    #[arg(long, default_value_t = 10)]
    interval: u64,

    /// Seconds to wait on one target before giving up on this round.
    ///
    /// A pump is O(work outstanding), so a slow one is normal under load and must
    /// not be able to stall every other target behind it.
    #[arg(long, default_value_t = 30)]
    timeout: u64,

    /// NATS, for the lease and the bus watch. Without it this relay fires alone.
    #[arg(long, env = "NATS_URL")]
    nats_url: Option<String>,

    /// Which lattice's lease to contend for.
    #[arg(long, default_value = "default")]
    lattice: String,

    /// Seconds a holder survives without renewing. Must be longer than `interval`,
    /// since the lease is renewed once per sweep.
    #[arg(long, default_value_t = 30)]
    lease_ttl: u64,

    /// Fire this relay alone, taking no lease. For a single-relay deployment that
    /// wants no lease bucket, and for tests that assert on one loop.
    #[arg(long)]
    no_lease: bool,

    /// Also watch the event bus's sequence keys and sweep early when one changes.
    ///
    /// The latency path. Needs `--nats-url`; the sweep keeps running either way.
    #[arg(long)]
    watch_bus: bool,

    /// The KV bucket `event-bus` writes its sequence keys in. `default` is what
    /// every component in this catalog opens.
    #[arg(long, default_value = "default")]
    bus_bucket: String,

    /// Take one sweep and exit. For a cron, and for proving the wiring without
    /// leaving something running — the same escape hatch `comp-goald` has.
    #[arg(long)]
    once: bool,
}

/// One pass over every target. Errors are reported and not fatal: a pump that is
/// down now is a pump that may be up in `interval` seconds, and a relay that exits
/// on the first refused connection is a relay that stops being a scheduler the first
/// time an app restarts.
async fn sweep(http: &reqwest::Client, targets: &[String], why: &str) {
    for t in targets {
        match http.post(t).send().await {
            Ok(r) if r.status().is_success() => {}
            Ok(r) => eprintln!("comp-relay: {t} answered {} [{why}]", r.status()),
            Err(e) => eprintln!("comp-relay: {t} unreachable: {e} [{why}]"),
        }
    }
}

/// Reject a target that is not a URL this process should be dialling.
///
/// Cheap, and it catches the mistake that would otherwise be discovered as a relay
/// that appears to run and pokes nothing.
fn check_targets(targets: &[String]) -> Result<()> {
    for t in targets {
        // `/internal/pump` is what every app calls the endpoint, so it is the most
        // likely thing to be pasted in. url's own message for it is "relative URL
        // without a base", which does not tell the reader what to type instead.
        if t.starts_with('/') {
            bail!("target {t:?} is a path, not a URL — name the app too, e.g. http://127.0.0.1:3012{t}");
        }
        let url = reqwest::Url::parse(t)
            .with_context(|| format!("target {t:?} is not a URL"))?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("target {t:?} must be http or https, not {:?}", url.scheme());
        }
        if url.host().is_none() {
            bail!("target {t:?} names no host");
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    check_targets(&args.targets)?;

    if args.lease_ttl <= args.interval && !args.no_lease && args.nats_url.is_some() {
        bail!(
            "lease-ttl {} is not longer than interval {} — the lease is renewed once per sweep, so it would expire between renewals",
            args.lease_ttl,
            args.interval
        );
    }
    if args.watch_bus && args.nats_url.is_none() {
        bail!("--watch-bus needs --nats-url: the sequence keys it watches live in NATS KV");
    }

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(args.timeout))
        .build()
        .context("building the HTTP client")?;

    if args.once {
        sweep(&http, &args.targets, "once").await;
        return Ok(());
    }

    // The lease, when there is a lattice to elect within. The id names the box and
    // the process so `holder()` is readable by a person looking for which relay is
    // acting.
    let mut lease = match (&args.nats_url, args.no_lease) {
        (Some(url), false) => {
            let id = format!(
                "{}:{}",
                hostname().unwrap_or_else(|| "unknown".into()),
                std::process::id()
            );
            // A relay is not a reconciler: it must contend for its OWN key, or the
            // two would evict each other and neither would run.
            let scope = format!("{}-relay", args.lattice);
            Some(
                comp_lattice::lease::Lease::connect(
                    url,
                    &scope,
                    Duration::from_secs(args.lease_ttl),
                    &id,
                )
                .await
                .context("opening the relay lease")?,
            )
        }
        _ => {
            eprintln!("comp-relay: no lease — this relay fires alone. Two of these would race a consumer group's offset.");
            None
        }
    };

    // The bus watch, when asked for. A change on any topic's seq key means somebody
    // published; which topic is not interesting, because the pump drains all of them.
    //
    // Dialled through `comp-lattice`, which is the only place in this workspace that
    // knows how to reach NATS — a second connect here would be a second place to get
    // multi-server failover wrong (ADR-0067).
    let bus = if args.watch_bus {
        let url = args.nats_url.as_deref().unwrap();
        Some(Box::pin(
            comp_lattice::nats::watch_bucket(url, &args.bus_bucket, "eb.seq.>")
                .await
                .context("watching the event bus")?,
        ))
    } else {
        None
    };
    let mut bus = bus;

    eprintln!(
        "comp-relay: {} target(s), sweeping every {}s{}",
        args.targets.len(),
        args.interval,
        if args.watch_bus { ", and early on any bus publish" } else { "" }
    );

    let mut tick = tokio::time::interval(Duration::from_secs(args.interval));
    // A sweep that takes longer than the interval must not queue up a burst of
    // catch-up sweeps behind it; the next one on schedule is what was wanted.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        // Only the holder acts. Renewal happens here, once per pass, which is why
        // the TTL must outlive the interval.
        let acting = match lease.as_mut() {
            Some(l) => l.hold().await,
            None => true,
        };

        tokio::select! {
            _ = tick.tick() => {
                if acting {
                    sweep(&http, &args.targets, "sweep").await;
                }
            }
            Some(_) = async { match bus.as_mut() { Some(w) => w.next().await, None => None } } => {
                // The latency path. Still gated on the lease: two relays draining one
                // consumer group is the race this whole mechanism exists to stop.
                if acting {
                    sweep(&http, &args.targets, "publish").await;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                if let Some(l) = lease.as_mut() {
                    // Hand it over now rather than making a standby wait out the TTL.
                    l.release().await;
                }
                return Ok(());
            }
        }
    }
}

fn hostname() -> Option<String> {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_that_is_not_a_url_is_refused_at_startup() {
        // The alternative is a relay that starts, looks healthy, and pokes nothing.
        assert!(check_targets(&["not a url".into()]).is_err());
        assert!(check_targets(&["ftp://box/pump".into()]).is_err());
        assert!(check_targets(&["http://127.0.0.1:3012/internal/pump".into()]).is_ok());
    }

    #[test]
    fn a_relative_path_is_refused_rather_than_resolved() {
        // "/internal/pump" is what the app calls the endpoint, and it is the most
        // likely thing to be pasted in. There is no base to resolve it against, so
        // saying so beats dialling something unintended.
        assert!(check_targets(&["/internal/pump".into()]).is_err());
    }
}
