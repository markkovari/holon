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

    /// Seconds between passes.
    #[arg(long, default_value = "10")]
    interval: u64,

    /// Consecutive passes a surplus must persist before anything is stopped.
    /// A flag, not a constant: it is a guess until there is real churn to
    /// calibrate it against.
    #[arg(long, default_value = "2")]
    settle_passes: u32,

    /// Commands per pass, so a mass event drains instead of stampeding.
    #[arg(long, default_value = "20")]
    max_commands: usize,

    /// Seconds to wait for a host to acknowledge a command.
    #[arg(long, default_value = "10")]
    command_timeout: u64,

    /// How long a host's inventory survives without a refresh. The reason a
    /// vanished node needs no reaping code: its key simply expires.
    #[arg(long, default_value = "15")]
    inventory_ttl: u64,

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
    let fabric = std::sync::Arc::new(
        NatsLattice::connect(&args.nats_url, &args.lattice, Duration::from_secs(args.inventory_ttl))
            .await?,
    );
    let inventory: std::sync::Arc<dyn Inventory> = fabric.clone();
    let commands: std::sync::Arc<dyn CommandBus> = fabric.clone();
    let artifacts: std::sync::Arc<dyn Artifacts> = fabric.clone();

    eprintln!(
        "comp-reconciler: lattice={} nats={} platform={} | every {}s{}",
        args.lattice,
        args.nats_url,
        args.platform_url,
        args.interval,
        if args.dry_run { " | DRY RUN, no commands will be sent" } else { "" }
    );

    let http = reqwest::Client::new();
    let cfg = Cfg { settle_passes: args.settle_passes, max_commands: args.max_commands };
    let mut hyst = Hysteresis::default();
    let period = Duration::from_secs(args.interval.max(1));

    loop {
        tokio::time::sleep(period).await;

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

        let observed = match fetch_inventory(inventory.as_ref()).await {
            Ok(o) => o,
            Err(e) => {
                // Same rule, other half. An unreadable inventory is not an empty
                // fleet; acting on one would restart everything everywhere.
                eprintln!("comp-reconciler: reading inventory failed: {e:#}");
                continue;
            }
        };

        let outcome = plan(&desired, &observed, &mut hyst, &cfg);
        report(&args, &http, &outcome).await;

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
            if let Err(e) = send(commands.as_ref(), &args, c).await {
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

async fn fetch_inventory(inventory: &dyn Inventory) -> Result<Vec<NodeInventory>> {
    let mut out = Vec::new();
    for entry in inventory.read_all().await? {
        match serde_json::from_slice::<NodeInventory>(&entry.value) {
            Ok(inv) => out.push(inv),
            Err(e) => {
                eprintln!("comp-reconciler: node {} wrote unreadable inventory: {e}", entry.key)
            }
        }
    }
    Ok(out)
}

async fn send(bus: &dyn CommandBus, args: &Args, cmd: &Command) -> Result<()> {
    let verb = match cmd {
        Command::Start { .. } => "start",
        Command::Stop { .. } => "stop",
    };
    // "Nothing is listening on that node" and "that node is slow" are kept distinct
    // by the implementation; both surface here as an error with the reason.
    let reply = bus
        .send(
            cmd.node(),
            verb,
            serde_json::to_vec(cmd)?,
            Duration::from_secs(args.command_timeout),
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
async fn report(args: &Args, http: &reqwest::Client, outcome: &Outcome) {
    if outcome.unschedulable.is_empty() {
        return;
    }
    for u in &outcome.unschedulable {
        eprintln!("comp-reconciler: {}/{} unschedulable: {}", u.tenant, u.app, u.reason);
    }
    let url = format!("{}/api/internal/status", args.platform_url.trim_end_matches('/'));
    let _ = http
        .post(&url)
        .header("x-platform-secret", &args.secret)
        .json(&json!({ "unschedulable": outcome.unschedulable }))
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
