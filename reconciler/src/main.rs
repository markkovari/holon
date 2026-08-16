//! `comp-reconciler` — the platform's only holder of a lattice credential.
//!
//! `platform-domain` (wasm) decides everything and stores a manifest per revision;
//! this makes the fleet match it. It exists because a wasm component has no
//! background: reconciling needs a held subscription and a timer. See docs/adr/0022.
//!
//! It holds no business logic, no database and no user concept, so it stays small
//! enough to audit in one sitting — which matters, because it is the process that
//! can start code on every node.
//!
//! The shape is deliberately the old applier's: poll the platform, derive
//! everything from what it says, and change nothing when the poll fails. What
//! changed underneath is the substrate, not the loop.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use comp_lattice::{nats::NatsLattice, Artifacts, CommandBus, Inventory};
use comp_reconciler::oci;
use comp_reconciler::settings;
use comp_reconciler::plan::{plan, Cfg, Command, Hysteresis, Manifest, NodeInventory, Outcome};
use serde_json::json;

#[derive(Parser, Clone)]
#[command(name = "comp-reconciler", about = "Makes the lattice match the platform's manifests")]
struct Args {
    /// The platform to poll for desired state.
    #[arg(long, env = "PLATFORM_URL")]
    platform_url: String,

    /// Shared secret presented as `x-platform-secret`.
    #[arg(long, env = "PLATFORM_SECRET")]
    secret: String,

    #[arg(long, env = "NATS_URL", default_value = "nats://127.0.0.1:4222")]
    nats_url: String,

    /// Lattice name, the first subject token after `comp.`. One control plane per
    /// lattice; two lattices on one NATS never see each other's commands.
    #[arg(long, default_value = "default")]
    lattice: String,

    /// Config file. Defaults to $COMP_CONFIG, then ./comp.toml if it exists.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Seconds between passes.
    #[arg(long, env = "COMP_INTERVAL")]
    interval: Option<u64>,

    /// Seconds a leader survives without renewing its lease.
    ///
    /// Failover takes up to this plus one interval, so it trades how long the
    /// fleet goes unreconciled against how long a network hiccup can hand
    /// leadership to a standby that did not need it. Must be comfortably longer
    /// than `--interval`, since the lease is renewed once per pass.
    #[arg(long, env = "COMP_LEASE_TTL", default_value = "30")]
    lease_ttl: u64,

    /// Reconcile without taking the lease. For a single-reconciler deployment
    /// that does not want a lease bucket, and for tests that assert on one loop.
    #[arg(long, env = "COMP_NO_LEASE")]
    no_lease: bool,

    /// Consecutive passes a surplus must persist before anything is stopped.
    /// A flag, not a constant: it is a guess until there is real churn to
    /// calibrate it against.
    #[arg(long, env = "COMP_SETTLE_PASSES")]
    settle_passes: Option<u32>,

    /// Re-rank the whole fleet for every app, every pass.
    ///
    /// The escape hatch for the converged fast path (ADR-0056). A differential
    /// test asserts the two agree, so this is for the day something disagrees in
    /// the field and you need the slow, obviously-correct behaviour NOW rather
    /// than after a diagnosis.
    #[arg(long, env = "COMP_NO_FAST_PATH")]
    no_fast_path: bool,

    /// Commands per pass, so a mass event drains instead of stampeding.
    #[arg(long, env = "COMP_MAX_COMMANDS")]
    max_commands: Option<usize>,

    /// Seconds to wait for a host to acknowledge a command.
    #[arg(long, env = "COMP_COMMAND_TIMEOUT")]
    command_timeout: Option<u64>,

    /// How long a host's inventory survives without a refresh. The reason a
    /// vanished node needs no reaping code: its key simply expires.
    #[arg(long, env = "COMP_INVENTORY_TTL")]
    inventory_ttl: Option<u64>,

    /// Compute and report, but send no commands and push nothing.
    #[arg(long)]
    dry_run: bool,

    /// Disable artifact distribution entirely.
    #[arg(long)]
    no_push: bool,

    /// Also mirror pushed artifacts to an OCI registry, as `host:port`. Off by
    /// default: nodes pull from the object store and need no registry at all.
    #[arg(long)]
    oci_mirror: Option<String>,

    #[arg(long, default_value = "http")]
    oci_scheme: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.secret.trim().is_empty() {
        anyhow::bail!(
            "--secret must not be empty: it is the only thing standing between this process's \
             credential and the network"
        );
    }

    // One implementation today; the loop below only ever sees the traits.
    // Resolve every tunable once: flag, then environment, then file, then default.
    let file = comp_reconciler::settings::File::load(args.config.as_deref())?.reconciler;
    let interval = settings::pick(args.interval, file.interval, 10);
    let settle_passes = settings::pick(args.settle_passes, file.settle_passes, 2);
    let max_commands = settings::pick(args.max_commands, file.max_commands, 20);
    let command_timeout = settings::pick(args.command_timeout, file.command_timeout, 10);
    let inventory_ttl = settings::pick(args.inventory_ttl, file.inventory_ttl, 15);

    let fabric = std::sync::Arc::new(
        NatsLattice::connect(&args.nats_url, &args.lattice, Duration::from_secs(inventory_ttl))
            .await?,
    );
    let inventory: std::sync::Arc<dyn Inventory> = fabric.clone();
    // The load bucket is optional on purpose: a fleet with no ingress publishing
    // (or an older one) reads as "no samples", and `desired_replicas` treats a
    // missing sample as "leave it alone" rather than as zero traffic.
    let load_in: Option<std::sync::Arc<dyn Inventory>> = NatsLattice::connect_bucket(
        &args.nats_url,
        &args.lattice,
        Duration::from_secs(30),
        comp_lattice::wire::LOAD,
    )
    .await
    .map(|l| std::sync::Arc::new(l) as std::sync::Arc<dyn Inventory>)
    .map_err(|e| {
        // Silently falling back to "no load samples" made autoscaling look broken
        // when it was never connected: the bucket had the signal, the ingress was
        // publishing, and this end simply never read it. An optional input still has
        // to say when it is absent.
        eprintln!("comp-reconciler: no load signal, autoscaling will not fire: {e:#}");
        e
    })
    .ok();
    let commands: std::sync::Arc<dyn CommandBus> = fabric.clone();
    let artifacts: std::sync::Arc<dyn Artifacts> = fabric.clone();

    eprintln!(
        "comp-reconciler: lattice={} nats={} platform={} | every {}s{}",
        args.lattice,
        args.nats_url,
        args.platform_url,
        interval,
        if args.dry_run { " | DRY RUN, no commands will be sent" } else { "" }
    );

    let http = reqwest::Client::new();
    let cfg = Cfg { settle_passes, max_commands, fast_path: !args.no_fast_path };

    // What the last pass knew, shared with the activation server below. Activation
    // must not re-poll the control plane: it sits in front of a user waiting on a
    // request, and an HTTP round trip to the platform would cost more than the start
    // it is trying to perform (ADR-0040 put a warm start at 0.43 ms).
    let known: std::sync::Arc<std::sync::RwLock<(Vec<Manifest>, Vec<NodeInventory>)>> =
        std::sync::Arc::new(std::sync::RwLock::new((Vec::new(), Vec::new())));
    if !args.dry_run {
        serve_activations(commands.clone(), known.clone(), cfg.clone(), args.clone(), command_timeout)
            .await;
    }
    let mut hyst = Hysteresis::default();
    let mut world = World::default();
    let period = Duration::from_secs(interval.max(1));

    // Exactly one reconciler acts at a time (ADR-0072). Not for throughput — a
    // steady pass is 46 ms at 1000 nodes — but because this was the only control
    // component with no standby, and because two loops disagree about scale-down:
    // the cooldown counter lives in each process's `Hysteresis`.
    let mut lease = if args.no_lease {
        None
    } else {
        let id = format!(
            "{}-{}",
            hostname().unwrap_or_else(|| "reconciler".into()),
            std::process::id()
        );
        match comp_lattice::lease::Lease::connect(
            &args.nats_url,
            &args.lattice,
            Duration::from_secs(args.lease_ttl.max(interval * 2)),
            &id,
        )
        .await
        {
            Ok(l) => Some(l),
            // Refusing to start would make the lease a new way to lose the whole
            // control plane. Carrying on alone is what a single reconciler did
            // before this existed, and it is said out loud rather than assumed.
            Err(e) => {
                eprintln!(
                    "comp-reconciler: no lease bucket ({e:#}) — running WITHOUT \
                     leader election. A second reconciler would fight this one."
                );
                None
            }
        }
    };
    // `None` until the first pass, so a process that STARTS as a standby says so.
    // With a plain `false` the first pass compares equal to the initial value and
    // logs nothing: an operator starting a second reconciler would see a banner
    // and then silence, indistinguishable from a hung process.
    let mut was_leader: Option<bool> = None;

    loop {
        tokio::time::sleep(period).await;

        // A standby does nothing at all: no distribution, no diff, no commands.
        // It holds no inventory cache and no hysteresis, so when it does take
        // over it starts a scale-down cooldown from zero — the safe direction,
        // since under-replication fires on the first pass that sees it.
        if let Some(l) = lease.as_mut() {
            let leader = l.hold().await;
            if was_leader != Some(leader) {
                if leader {
                    eprintln!("comp-reconciler: this process is now the leader ({})", l.id());
                } else {
                    let who = l.holder().await.unwrap_or_else(|| "someone else".into());
                    eprintln!("comp-reconciler: standing by — {who} holds the lease");
                }
                was_leader = Some(leader);
            }
            if !leader {
                continue;
            }
        }

        // Distribute before reconciling, in the same pass. A manifest references an
        // artifact by digest, so a component whose bytes are not in the store yet
        // cannot start at all — distributing first means one pass takes an upload
        // all the way to running, instead of two.
        if !args.no_push && !args.dry_run {
            match push_pass(&args, &http, artifacts.as_ref()).await {
                Ok(0) => {}
                Ok(n) => eprintln!("comp-reconciler: distributed {n} artifact(s)"),
                Err(e) => eprintln!("comp-reconciler: distribution pass failed: {e:#}"),
            }
        }

        // A failed poll means we know nothing, so we change nothing. This is the
        // single most load-bearing line in the loop and it long predates the
        // lattice: treating "the poll failed" as "no apps exist" would stop every
        // running instance on the fleet.
        let Some(desired) = fetch_manifests(&args, &http).await else { continue };

        let observed = match world.refresh(inventory.as_ref()).await {
            Ok(o) => o,
            Err(e) => {
                // Same rule, other half. An unreadable inventory is not an empty
                // fleet; acting on one would restart everything everywhere.
                eprintln!("comp-reconciler: reading inventory failed: {e:#}");
                continue;
            }
        };

        // An unreadable load bucket is an empty sample set, NOT a reason to skip the
        // pass: autoscaling is an enhancement to the diff, and a manifest with fixed
        // replicas must keep reconciling whether or not anyone is publishing load.
        // None means the SIGNAL is absent — a different thing from an empty sample
        // set, and `plan` treats them differently: no signal holds every autoscaled
        // app where it is, rather than shrinking it back to the manifest count at
        // the moment nobody can see what it is carrying.
        let load = match &load_in {
            Some(l) => read_load(l.as_ref()).await,
            None => None,
        };

        *known.write().unwrap() = (desired.clone(), observed.to_vec());

        let outcome = plan(&desired, &observed, load.as_ref(), &mut hyst, &cfg);

        // How far behind the fleet is: every replica the manifests ask for,
        // against every instance the nodes say they are running. This is the
        // number the platform admits new work against, so it is computed on
        // every pass rather than only when something is stuck.
        let wanted: u64 = desired
            .iter()
            .flat_map(|m| m.components.iter())
            .map(|c| c.replicas.max(1) as u64)
            .sum();
        let running: u64 =
            observed.iter().map(|n| n.instances.iter().map(|i| i.count.max(1) as u64).sum::<u64>()).sum();
        let lag = wanted.saturating_sub(running);
        let nodes = observed.len() as u64;
        report(&args, &http, &outcome, lag, wanted, running, nodes).await;

        if outcome.commands.is_empty() {
            continue;
        }
        if outcome.deferred > 0 {
            eprintln!(
                "comp-reconciler: {} command(s) this pass, {} deferred to the next",
                outcome.commands.len(),
                outcome.deferred
            );
        }
        if args.dry_run {
            for c in &outcome.commands {
                eprintln!("comp-reconciler: would send {}", describe(c));
            }
            continue;
        }
        for c in &outcome.commands {
            if let Err(e) = send(commands.as_ref(), &args, &http, command_timeout, c).await {
                // Logged and dropped on purpose. The next pass re-derives from
                // scratch, so a failed command costs one interval — cheaper and far
                // more predictable than a per-command retry state machine.
                eprintln!("comp-reconciler: {} failed: {e:#}", describe(c));
            }
        }
    }
}

fn describe(c: &Command) -> String {
    match c {
        Command::Start { node, tenant, app, component, count, .. } => {
            format!("start {tenant}/{app}/{component} ×{count} on {node}")
        }
        Command::Stop { node, tenant, app, component, count, .. } => {
            format!("stop {tenant}/{app}/{component} ×{count} on {node}")
        }
    }
}

/// Desired state. `None` means "we learned nothing this pass" and is never the
/// same as "there is nothing to run" — see the call site.
async fn fetch_manifests(args: &Args, http: &reqwest::Client) -> Option<Vec<Manifest>> {
    let url = format!("{}/api/internal/revisions", args.platform_url.trim_end_matches('/'));
    let body = match http.get(&url).header("x-platform-secret", &args.secret).send().await {
        Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.ok()?,
        Ok(r) => {
            eprintln!("comp-reconciler: revisions poll got {}", r.status());
            return None;
        }
        Err(e) => {
            eprintln!("comp-reconciler: revisions poll failed: {e}");
            return None;
        }
    };

    let mut out = Vec::new();
    for rev in body["revisions"].as_array().cloned().unwrap_or_default() {
        match serde_json::from_value::<Manifest>(rev["manifest"].clone()) {
            Ok(m) => out.push(m),
            // A manifest we cannot parse is a platform bug, and skipping it would
            // read as "this app was deleted" and stop it. Refuse the whole pass
            // instead — one broken record must not take an app down.
            Err(e) => {
                eprintln!(
                    "comp-reconciler: revision {} has an unreadable manifest ({e}) — \
                     changing nothing this pass",
                    rev["id"].as_str().unwrap_or("?")
                );
                return None;
            }
        }
    }
    Some(out)
}

/// The parsed world, reused across passes.
///
/// A node's snapshot is byte-identical between passes unless that node actually
/// did something, and re-parsing 20 000 instances of JSON to learn that costs
/// 4.4 ms of a 45 ms pass. Hashing the bytes answers the same question in 0.6 ms
/// (ADR-0058).
///
/// Deliberately NOT a watch subscription, which was the obvious alternative:
/// `read_all` returns what has not expired, so a node's absence IS its death and
/// no liveness logic exists anywhere. A watched mirror would have to re-derive
/// expiry locally and resync periodically in case it missed an event — new
/// machinery, a new way to be wrong about which machines are alive, to save the
/// same 4 ms this saves with a hash.
#[derive(Default)]
struct World {
    /// Sorted by node, so `plan`'s output stays deterministic.
    nodes: Vec<NodeInventory>,
    /// node key -> hash of the bytes it last published.
    seen: std::collections::HashMap<String, u64>,
}

fn hash_of(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

impl World {
    async fn refresh(&mut self, inventory: &dyn Inventory) -> Result<&[NodeInventory]> {
        let entries = inventory.read_all().await?;
        let mut live = std::collections::HashSet::with_capacity(entries.len());

        for entry in &entries {
            live.insert(entry.key.clone());
            let h = hash_of(&entry.value);
            if self.seen.get(&entry.key) == Some(&h) {
                continue;
            }
            match serde_json::from_slice::<NodeInventory>(&entry.value) {
                Ok(inv) => {
                    self.seen.insert(entry.key.clone(), h);
                    match self.nodes.iter_mut().find(|n| n.node == inv.node) {
                        Some(slot) => *slot = inv,
                        None => self.nodes.push(inv),
                    }
                }
                // Unreadable is not empty: the previous good snapshot stays, which
                // is the same instinct as refusing a pass on an unreadable
                // manifest rather than reading it as a deletion.
                Err(e) => {
                    eprintln!("comp-reconciler: node {} wrote unreadable inventory: {e}", entry.key)
                }
            }
        }

        // Gone from the bucket means expired means dead. This is the ONLY liveness
        // signal in the system and it must not be cached.
        self.seen.retain(|k, _| live.contains(k));
        self.nodes.retain(|n| live.contains(&n.node));
        self.nodes.sort_by(|a, b| a.node.cmp(&b.node));
        Ok(&self.nodes)
    }
}

/// Turn a planned command into the bytes a host receives.
///
/// A start that grants secrets picks up two extra fields here and nowhere else: the
/// `key -> ref` map the host looks guest strings up in, and a token authorising
/// exactly those references. The reconciler never sees a secret VALUE — it asks the
/// platform for a capability and passes it on (ADR-0051).
async fn wire_command(
    args: &Args,
    http: &reqwest::Client,
    cmd: &Command,
) -> Result<serde_json::Value> {
    let mut body = serde_json::to_value(cmd)?;
    let Command::Start { secrets, tenant, app, component, node, .. } = cmd else {
        return Ok(body);
    };

    // ALWAYS rewritten, even when empty. The planner carries secrets as a list and
    // the host looks them up as a map, so the shapes differ — and converting only
    // the non-empty case left every ordinary start sending a list into a field the
    // host reads as a map. It refused all of them: "invalid type: sequence,
    // expected a map". Caught by the e2e suite on the first run after the change,
    // which is what it is for.
    body["secrets"] = json!(secrets
        .iter()
        .map(|s| (s.key.clone(), s.reference.clone()))
        .collect::<std::collections::BTreeMap<_, _>>());
    // Minted even with no secrets: the token is this instance's proof of identity,
    // which is what lets a component ask the platform to fork its own app
    // (ADR-0079) without the host holding a credential that could touch anyone
    // else's. A token with no refs authorises no secret.

    let instance = format!("{tenant}/{app}/{component}@{node}");
    let refs: Vec<&str> = secrets.iter().map(|s| s.reference.as_str()).collect();
    let url = format!("{}/api/internal/fetch-token", args.platform_url.trim_end_matches('/'));
    // Hard for secrets, soft for identity. An instance that was GRANTED secrets and
    // cannot get a token must not start — it would fail at the first reveal, in
    // front of a user (ADR-0061). An instance with no secrets wants the token only
    // to prove who it is when it asks to fork its own app (ADR-0079), and refusing
    // to start it would make a brand-new optional capability able to take the
    // whole fleet down. It did, for one commit: every start began minting, and a
    // control plane without that route served nothing at all.
    let need = !secrets.is_empty();
    let minted = async {
        let res = http
            .post(&url)
            .header("x-platform-secret", &args.secret)
            .json(&json!({ "instance": instance, "refs": refs }))
            .send()
            .await
            .context("asking for an instance token")?;
        if !res.status().is_success() {
            anyhow::bail!("the platform refused an instance token: {}", res.status());
        }
        let token = res.json::<serde_json::Value>().await.context("reading the token")?;
        let token = token["token"].as_str().unwrap_or_default().to_string();
        if token.is_empty() {
            anyhow::bail!("the platform returned an empty token");
        }
        Ok::<_, anyhow::Error>(token)
    }
    .await;
    match minted {
        Ok(token) => body["fetch_token"] = json!(token),
        Err(e) if need => return Err(e),
        Err(e) => {
            // Said once per start rather than swallowed: an instance without a
            // token cannot spawn, and finding that out from silence is worse.
            eprintln!("comp-reconciler: {instance} starts without an instance token ({e:#})");
        }
    }
    Ok(body)
}

async fn send(
    bus: &dyn CommandBus,
    args: &Args,
    http: &reqwest::Client,
    command_timeout: u64,
    cmd: &Command,
) -> Result<()> {
    let verb = match cmd {
        Command::Start { .. } => "start",
        Command::Stop { .. } => "stop",
    };
    let body = wire_command(args, http, cmd).await?;
    // "Nothing is listening on that node" and "that node is slow" are kept distinct
    // by the implementation; both surface here as an error with the reason.
    let reply = bus
        .send(
            cmd.node(),
            verb,
            serde_json::to_vec(&body)?,
            Duration::from_secs(command_timeout),
        )
        .await?;

    let ack: serde_json::Value = serde_json::from_slice(&reply).unwrap_or_default();
    if let Some(err) = ack["error"].as_str() {
        anyhow::bail!("host refused: {err}");
    }
    Ok(())
}

/// Tell the platform what could not be placed. One endpoint, so an app stuck
/// unschedulable is visible in the UI instead of only in these logs.
#[allow(clippy::too_many_arguments)]
async fn report(
    args: &Args,
    http: &reqwest::Client,
    outcome: &Outcome,
    lag: u64,
    desired: u64,
    placed: u64,
    nodes: u64,
) {
    // Said out loud even though nothing here can fix it: `max` is the operator's
    // limit, so a component pinned against it with demand still arriving is a
    // decision only they can make. Silence would make it indistinguishable from an
    // app that is correctly sized.
    for c in &outcome.at_ceiling {
        eprintln!(
            "comp-reconciler: {}/{}/{} is at its ceiling of {} replicas and demand asked for {} — raise `scale.max` or accept the shedding",
            c.tenant, c.app, c.component, c.max, c.wanted
        );
    }
    // NO early return when nothing is stuck. This function used to bail out here,
    // which was right when its only job was surfacing problems and wrong the
    // moment the platform started admitting work against the lag: a fleet with
    // nothing wrong reported nothing at all, so admission saw a report that never
    // arrived and, after the grace period, failed closed on a perfectly healthy
    // fleet. The lag is most useful precisely when it is small.
    for u in &outcome.unschedulable {
        eprintln!("comp-reconciler: {}/{} unschedulable: {}", u.tenant, u.app, u.reason);
    }
    let url = format!("{}/api/internal/status", args.platform_url.trim_end_matches('/'));
    let _ = http
        .post(&url)
        .header("x-platform-secret", &args.secret)
        // The NUMBERS, not only the problems.
        //
        // `platform-domain`'s `fleet_lag()` reads `lag` and `nodes` off this row
        // and `admit_one_more()` sizes its limit from them — and neither field was
        // ever sent, so the lag it admitted against was permanently 0 and the
        // per-node limit was permanently `per_node × 1`. Half of admission control
        // was measuring a number nobody reported.
        //
        // rustc had been saying so for a while: these four arguments were computed,
        // passed in, and then unused, which is what an unimplemented half of a
        // feature looks like from inside the compiler.
        .json(&json!({
            "lag": lag,
            "desired": desired,
            "placed": placed,
            "nodes": nodes,
            "unschedulable": outcome.unschedulable,
            "at_ceiling": outcome.at_ceiling,
        }))
        .send()
        .await;
}

/// One pass of the distribution queue: ask what has no content address yet, put the
/// bytes in the object store, report the digest back.
///
/// Everything about it is idempotent — "pending" is derived from the absence of a
/// digest, and the object store is content-addressed — so a crash anywhere in here
/// costs a repeated upload, never a wrong one.
async fn push_pass(
    args: &Args,
    http: &reqwest::Client,
    store: &dyn Artifacts,
) -> Result<usize> {
    let base = args.platform_url.trim_end_matches('/');
    let pending = http
        .get(format!("{base}/api/internal/pending-pushes"))
        .header("x-platform-secret", &args.secret)
        .send()
        .await
        .context("asking for pending pushes")?
        .json::<serde_json::Value>()
        .await
        .context("parsing pending pushes")?;

    let mut pushed = 0usize;
    for row in pending["pending"].as_array().cloned().unwrap_or_default() {
        let Some(key) = row["key"].as_str() else { continue };
        let bytes = match http
            .get(format!("{base}/api/internal/artifact?key={key}"))
            .header("x-platform-secret", &args.secret)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r.bytes().await.ok().map(|b| b.to_vec()),
            Ok(r) => {
                eprintln!("comp-reconciler: artifact {key} got {}", r.status());
                None
            }
            Err(e) => {
                eprintln!("comp-reconciler: artifact {key} failed: {e}");
                None
            }
        };
        let Some(bytes) = bytes else { continue };

        // A corruption check on the fetch, not an authenticity check — and a PREFIX
        // comparison, because the catalog's `sha256` is `wit:reflect`'s 12-char
        // display hash, not a full digest. 48 bits is plenty to catch a truncated or
        // mangled transfer, which is the failure this guards.
        //
        // Found by this check firing on its first real run, which is the argument for
        // having written it.
        if let Some(want) = row["sha256"].as_str() {
            let want = want.trim_start_matches("sha256:");
            let got = oci::sha256_hex(&bytes);
            if want.is_empty() || !got.starts_with(want) {
                eprintln!(
                    "comp-reconciler: {key} does not match the catalog (expected sha256 to start \
                     {want}, fetched {got}) — not distributing"
                );
                continue;
            }
        }

        let digest = oci::digest_of(&bytes);
        if !store.has(&digest).await {
            store
                .put(&digest, bytes.clone())
                .await
                .with_context(|| format!("storing {key} as {digest}"))?;
        }

        if let Some(registry) = &args.oci_mirror {
            let repo = row["repo"].as_str().unwrap_or(key);
            let strings = |v: &serde_json::Value| -> Vec<String> {
                v.as_array()
                    .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                    .unwrap_or_default()
            };
            if let Err(e) = oci::push_artifact(
                http,
                &format!("{}://{registry}", args.oci_scheme),
                repo,
                &bytes,
                &strings(&row["exports"]),
                &strings(&row["imports"]),
            )
            .await
            {
                // The mirror is a convenience, not the distribution path. A failure
                // here must not stop the component from being deployable.
                eprintln!("comp-reconciler: mirroring {repo} to OCI failed: {e:#}");
            }
        }

        let res = http
            .post(format!("{base}/api/internal/pushed"))
            .header("x-platform-secret", &args.secret)
            .json(&json!({ "key": key, "digest": digest }))
            .send()
            .await;
        match res {
            Ok(r) if r.status().is_success() => {
                eprintln!("comp-reconciler: distributed {key} {digest}");
                pushed += 1;
            }
            // The bytes landed but the platform did not record it. Harmless: the
            // component stays pending and the next pass repeats, which is
            // content-addressed and therefore free.
            Ok(r) => eprintln!("comp-reconciler: stored {key} but /pushed got {}", r.status()),
            Err(e) => eprintln!("comp-reconciler: stored {key} but /pushed failed: {e}"),
        }
    }
    Ok(pushed)
}

#[cfg(test)]
mod tests {
    use comp_reconciler::plan::{Component, Ingress, Manifest, Placement, Strategy};

    /// The manifest has to survive the platform → reconciler hop by value, since it
    /// is what replaced a rendered YAML string. A field silently defaulting here
    /// would place an app somewhere nobody asked for.
    #[test]
    fn a_manifest_round_trips_through_json() {
        let m = Manifest {
            app: "mesh".into(),
            tenant: "alice".into(),
            strategy: Strategy::Linked,
            components: vec![Component {
                scale: None,
                id: "api".into(),
                digest: "sha256:abc".into(),
                replicas: 2,
                placement: Placement {
                    mode: comp_reconciler::plan::Mode::Spread,
                    nodes: vec![],
                    constraints: [("region".to_string(), "eu-central".to_string())].into(),
                },
                host_needs: vec!["wasi:keyvalue/store@0.2.0-draft".into()],
                config: [("grace-period-secs".to_string(), "5".to_string())].into(),
                secrets: vec![],
                egress: vec!["api.stripe.com".into()],
            }],
            links: vec![],
            ingress: Some(Ingress { host: "mesh.example.com".into(), component: "api".into() }),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Manifest>(&json).unwrap(), m);
        // The wire form the platform actually writes.
        assert!(json.contains(r#""strategy":"linked""#), "{json}");
        assert!(json.contains(r#""mode":"spread""#), "{json}");
    }

    /// A minimal manifest must not need every field spelled out, or the platform's
    /// writer and this reader drift the first time one gains a field.
    #[test]
    fn optional_fields_default() {
        let m: Manifest = serde_json::from_str(
            r#"{"app":"a","tenant":"t","strategy":"fused",
                "components":[{"id":"x","digest":"sha256:d"}]}"#,
        )
        .expect("parses");
        assert_eq!(m.components[0].replicas, 1);
        assert!(m.links.is_empty());
        assert!(m.ingress.is_none());
    }
}

/// Observed demand per ingress host. Several ingresses may publish; their samples are
/// SUMMED, because two ingresses each seeing 5 in flight means the app is carrying
/// 10, not 5.
async fn read_load(load: &dyn Inventory) -> Option<comp_reconciler::plan::Load> {
    match load.read_all().await {
        Ok(entries) => Some(fold_load(&entries)),
        // A bucket that cannot be read is the signal being absent, not the fleet
        // being idle. Returning an empty map here would shrink every autoscaled app.
        Err(e) => {
            eprintln!("comp-reconciler: load signal unreadable this pass: {e:#}");
            None
        }
    }
}

/// Demand is in-flight PLUS refused.
///
/// Shedding (ADR-0041) creates a blind spot in the autoscaling signal: a shed request
/// never becomes in-flight, so concurrency understates demand exactly when demand is
/// highest. Left alone, the two features fight — the ingress refuses traffic while
/// the reconciler sees a calm app and declines to grow it.
///
/// Counting a refusal as one unit of unmet concurrency is deliberately crude. It is
/// not a measurement of how much load was turned away, and it does not need to be:
/// its job is to push `desired` upward while refusals continue, and `max` is where it
/// stops. An app that is shedding should go to its ceiling; that is what the ceiling
/// is for.
///
/// Pure, so the arithmetic can be tested without a bus.
fn fold_load(entries: &[comp_lattice::Entry]) -> comp_reconciler::plan::Load {
    let mut out = comp_reconciler::plan::Load::new();
    for e in entries {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&e.value) else { continue };
        let Some(host) = v["host"].as_str() else { continue };
        let inflight = v["inflight"].as_u64().unwrap_or(0);
        // Absent on an older ingress, which must read as "no refusals" rather than
        // as a parse failure that drops the whole sample.
        let shed = v["shed"].as_u64().unwrap_or(0);
        // Absent on an older ingress too. Missing reads as "assume it is serving",
        // which keeps the previous behaviour for a mixed fleet mid-rollout.
        let served = v["served"].as_u64().unwrap_or(u64::MAX);

        // Refusals only count as demand when the fleet is ANSWERING. A component
        // that is wedged — every replica holding connections it never completes —
        // pins in-flight at the bound and sheds everything behind it, which looks
        // exactly like honest saturation. Scaling that up manufactures more wedged
        // instances and makes the outage bigger.
        let demand = if served == 0 && shed > 0 { inflight } else { inflight + shed };
        *out.entry(host.to_string()).or_default() += demand as u32;
    }
    out
}

/// Answer "someone is asking for an app that has no replica placed".
///
/// The ingress cannot do this itself: it holds no platform credential and no
/// manifest, by design (ADR-0026). So it asks here, and this replies with an address
/// it can use immediately — waiting for the next inventory refresh instead would put
/// a heartbeat interval in front of a request, which is the whole thing ADR-0037's
/// 0.43 ms start makes worth avoiding.
///
/// Modelled as a command to a pseudo-node called `reconciler`, so it reuses the
/// existing command bus, subject naming and reply plumbing without a fourth trait.
///
/// ponytail: `serve` is a plain subscription, so two reconcilers would both act on
/// one activation. Harmless (a start is idempotent — absolute counts, ADR-0022) but
/// wasteful; make it a queue group when a second reconciler is real.
async fn serve_activations(
    bus: std::sync::Arc<dyn CommandBus>,
    known: std::sync::Arc<std::sync::RwLock<(Vec<Manifest>, Vec<NodeInventory>)>>,
    cfg: Cfg,
    args: Args,
    command_timeout: u64,
) {
    let http = reqwest::Client::new();
    let mut rx = match bus.serve("reconciler").await {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!("comp-reconciler: not serving activations: {e:#}");
            return;
        }
    };
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            if cmd.verb != "activate" {
                let _ = cmd.reply.send(br#"{"error":"unknown verb"}"#.to_vec());
                continue;
            }
            let host = serde_json::from_slice::<serde_json::Value>(&cmd.payload)
                .ok()
                .and_then(|v| v["host"].as_str().map(str::to_string))
                .unwrap_or_default();
            let body = match activate(
                bus.as_ref(), &known, &cfg, &args, &http, command_timeout, &host,
            )
            .await
            {
                Ok(v) => v,
                Err(e) => serde_json::json!({ "error": format!("{e:#}") }),
            };
            let _ = cmd.reply.send(body.to_string().into_bytes());
        }
    });
}

async fn activate(
    bus: &dyn CommandBus,
    known: &std::sync::RwLock<(Vec<Manifest>, Vec<NodeInventory>)>,
    cfg: &Cfg,
    args: &Args,
    http: &reqwest::Client,
    command_timeout: u64,
    host: &str,
) -> Result<serde_json::Value> {
    let (desired, observed) = known.read().unwrap().clone();
    // Only the app that owns this hostname, so an activation can never start
    // something the caller did not ask for.
    let mine: Vec<Manifest> = desired
        .iter()
        .filter(|m| m.ingress.as_ref().is_some_and(|i| i.host == host))
        .cloned()
        .collect();
    if mine.is_empty() {
        anyhow::bail!("no app answers to {host:?}");
    }

    // One in flight is exactly the signal that would have made the next pass place a
    // replica, so `plan` decides — placement rules, stateful-spread refusal and all —
    // rather than a second scheduler living here.
    let load = comp_reconciler::plan::Load::from([(host.to_string(), 1u32)]);
    let outcome = plan(
        &mine,
        &observed,
        Some(&load),
        &mut Hysteresis::default(),
        &Cfg { max_commands: cfg.max_commands.max(1), ..cfg.clone() },
    );
    if let Some(u) = outcome.unschedulable.first() {
        anyhow::bail!("{}", u.reason);
    }
    let start = outcome
        .commands
        .iter()
        .find(|c| matches!(c, Command::Start { .. }))
        .context("already placed, or nothing to start")?;
    send(bus, args, http, command_timeout, start).await?;

    let node = start.node().to_string();
    let address = observed
        .iter()
        .find(|n| n.node == node)
        .map(|n| n.address.clone())
        .context("started on a node with no advertised address")?;
    eprintln!("comp-reconciler: activated {host} on {node}");
    Ok(serde_json::json!({ "node": node, "address": address }))
}


#[cfg(test)]
mod load_tests {
    use super::*;
    use comp_lattice::Entry;

    fn entry(json: &str) -> Entry {
        Entry { key: "k".into(), value: json.as_bytes().to_vec() }
    }

    #[test]
    fn demand_counts_refusals_as_well_as_requests_in_flight() {
        // THE point of this change: an app being shed is an app that needs more
        // replicas, and in-flight alone cannot say so.
        let load = fold_load(&[entry(r#"{"host":"a.test","inflight":3,"shed":40}"#)]);
        assert_eq!(load.get("a.test"), Some(&43));
    }

    #[test]
    fn several_ingresses_are_summed() {
        // Two ingresses each seeing 5 means the app carries 10, not 5.
        let load = fold_load(&[
            entry(r#"{"host":"a.test","inflight":5,"shed":0}"#),
            entry(r#"{"host":"a.test","inflight":5,"shed":2}"#),
        ]);
        assert_eq!(load.get("a.test"), Some(&12));
    }

    #[test]
    fn an_older_ingress_without_the_field_still_counts() {
        // A mixed fleet during a rollout is normal (ADR-0044). A missing `shed` must
        // read as zero refusals, not as an unparseable sample to be dropped.
        let load = fold_load(&[entry(r#"{"host":"a.test","inflight":7}"#)]);
        assert_eq!(load.get("a.test"), Some(&7));
    }

    #[test]
    fn junk_is_skipped_without_losing_the_rest() {
        let load = fold_load(&[
            entry("not json at all"),
            entry(r#"{"nohost":true}"#),
            entry(r#"{"host":"b.test","inflight":1,"shed":1}"#),
        ]);
        assert_eq!(load.get("b.test"), Some(&2));
        assert_eq!(load.len(), 1);
    }

    #[test]
    fn a_wedged_component_does_not_ask_for_more_of_itself() {
        // Every replica holding a connection it never completes: in-flight pinned at
        // the bound, everything behind it shed, and nothing served. That is
        // indistinguishable from real saturation by shed count alone — and scaling
        // it up would manufacture more wedged instances.
        let load = fold_load(&[entry(r#"{"host":"a.test","inflight":8,"shed":5000,"served":0}"#)]);
        assert_eq!(load.get("a.test"), Some(&8), "hold at in-flight, do not add refusals");
    }

    #[test]
    fn a_genuinely_busy_component_still_counts_its_refusals() {
        let load = fold_load(&[entry(r#"{"host":"a.test","inflight":8,"shed":5000,"served":9000}"#)]);
        assert_eq!(load.get("a.test"), Some(&5008));
    }

    #[test]
    fn an_ingress_that_does_not_publish_served_keeps_the_old_behaviour() {
        // Mixed versions during a rollout are normal (ADR-0044). An older ingress
        // omits `served`, and its refusals must still count rather than being
        // silently discarded as "wedged".
        let load = fold_load(&[entry(r#"{"host":"a.test","inflight":2,"shed":30}"#)]);
        assert_eq!(load.get("a.test"), Some(&32));
    }

}

/// Who this process is, for the lease. Hostname plus pid, because two
/// reconcilers on one box during a rolling restart are the case that matters.
fn hostname() -> Option<String> {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
