//! `comp-relay` — what turns a pull contract into a trigger.
//!
//! ## The gap this fills
//!
//! `sched:timer`, `event:bus` and `cron:expr` describe work that happens *later* and are deliberately
//! PULL (pure WASI, any host). Something outside must poke the app: on Kubernetes a curl-loop
//! Deployment, on the native lane the operator refreshing a browser tab. This is that poke, as one
//! small daemon.
//!
//! ## Why this is native, per ADR-0095
//!
//! A `wasm32-wasip2` component has no background (it exists between a request and its response), and
//! this needs a timer that runs with no request in flight and a held NATS subscription. It decides
//! only WHEN to poke, never what to do: eligibility, recurrence and leasing stay in `scheduler-timer`,
//! offsets in `event-bus`, the work in the app's own `pump`. What it speaks is the app's existing
//! `wasi:http/incoming-handler`, so no app changes to gain a trigger.
//!
//! ## What it actually does
//!
//! `POST` to an endpoint, on a schedule. `/internal/pump` is an established convention here
//! (`saga-domain` and the four `eshop-*` services export it), so the relay needs no WIT bindings and
//! no knowledge of what a job is.
//!
//! ## An allow-list, for the reason `comp-fswatch` has one
//!
//! A target is a URL from configuration, a request this process makes on behalf of whoever wrote it:
//! a poker aimed at an arbitrary host is a confused deputy with a timer. So every target is named on
//! the command line, with no wildcard and no default; a relay with no `--target` refuses to start
//! rather than idle, since a scheduler with nothing scheduled is nearly always a misconfiguration.
//!
//! ## Two paths, and why both
//!
//! **The sweep** is the correctness path: every target, every `--interval` seconds. It drives
//! time-based transitions no event announces (a grace period expiring) and catches up after downtime.
//!
//! **The push** is the latency path, only on a lattice: `event-bus` bumps a dotted seq key per topic in
//! NATS KV on every publish, so `$KV.<bucket>.eb.seq.>` is a change feed. It does NOT replace the
//! sweep: core-NATS delivery is at-most-once, so a restarting relay drops notifications, and no KV
//! change announces the passage of time. Push is an optimisation; the sweep is the contract.
//!
//! ## One firing relay per lattice
//!
//! With `--nats-url`, a JetStream KV lease decides which relay acts, as the reconciler's does
//! (`lattice/src/lease.rs`). It matters more here: `bus.poll` ADVANCES A CONSUMER GROUP'S OFFSET, so
//! two relays draining one group race and at-least-once silently becomes at-most-once. Without
//! `--nats-url` it fires alone and says so (right for tier 1, one box one app).
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
    name = "holon-relay",
    about = "Poke an app's pump endpoint on a schedule, so pull-based timers and topics fire."
)]
struct Args {
    /// An endpoint to POST, repeatable. Usually `<app>/internal/pump`.
    ///
    /// An allow-list, not a discovery mechanism: a target it inferred is a request nobody authorised.
    #[arg(long = "target", required = true)]
    targets: Vec<String>,

    /// Seconds between sweeps. The completeness path: short enough that a missed push is a delay, not a
    /// stall.
    #[arg(long, default_value_t = 10)]
    interval: u64,

    /// Seconds to wait on one target before giving up on this round.
    ///
    /// A pump is O(work outstanding), so a slow one is normal under load and must not stall the others.
    #[arg(long, default_value_t = 30)]
    timeout: u64,

    /// NATS, for the lease and the bus watch. Without it this relay fires alone.
    #[arg(long, env = "NATS_URL")]
    nats_url: Option<String>,

    /// Which lattice's lease to contend for.
    #[arg(long, default_value = "default")]
    lattice: String,

    /// Seconds a holder survives without renewing. Must be longer than `interval`, since the lease is
    /// renewed once per sweep.
    #[arg(long, default_value_t = 30)]
    lease_ttl: u64,

    /// Fire this relay alone, taking no lease. For a single-relay deployment and for tests.
    #[arg(long)]
    no_lease: bool,

    /// Also watch the event bus's sequence keys and sweep early when one changes (the latency path; needs
    /// `--nats-url`).
    #[arg(long)]
    watch_bus: bool,

    /// The KV bucket `event-bus` writes its sequence keys in; `default` is what every component opens.
    #[arg(long, default_value = "default")]
    bus_bucket: String,

    /// Take one sweep and exit. For a cron, and for proving the wiring.
    #[arg(long)]
    once: bool,
}

/// One pass over every target. Errors are reported, not fatal: a relay that exits on the first
/// refused connection stops being a scheduler the first time an app restarts.
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
/// Cheap, and catches the mistake that would otherwise surface as a relay that pokes nothing.
fn check_targets(targets: &[String]) -> Result<()> {
    for t in targets {
        // `/internal/pump` is the most likely thing to be pasted in, and url's own message ("relative URL
        // without a base") does not say what to type instead.
        if t.starts_with('/') {
            bail!("target {t:?} is a path, not a URL — name the app too, e.g. http://127.0.0.1:3012{t}");
        }
        let url = reqwest::Url::parse(t).with_context(|| format!("target {t:?} is not a URL"))?;
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

    // The lease, when there is a lattice to elect within. The id names the box and the process so
    // `holder()` shows which relay is acting.
    let mut lease = match (&args.nats_url, args.no_lease) {
        (Some(url), false) => {
            let id = format!(
                "{}:{}",
                hostname().unwrap_or_else(|| "unknown".into()),
                std::process::id()
            );
            // A relay is not a reconciler: it must contend for its OWN key, or the two would evict each other.
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

    // The bus watch, when asked for. A change on any seq key means somebody published; the pump drains
    // all topics, so which one does not matter.
    // Dialled through `comp-lattice`, the only place that knows how to reach NATS (multi-server failover,
    // ADR-0067).
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
    // A sweep that takes longer than the interval must not queue a burst of catch-up sweeps.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut shutdown = comp_lattice::lease::ShutdownListener::new();
    loop {
        // Only the holder acts. Renewal happens here, once per pass, so the TTL must outlive the interval.
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
                // The latency path, still gated on the lease: two relays draining one consumer group is the race this
                // mechanism exists to stop.
                if acting {
                    sweep(&http, &args.targets, "publish").await;
                }
            }
            _ = shutdown.wait() => {
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
        assert!(check_targets(&["/internal/pump".into()]).is_err());
    }
}
