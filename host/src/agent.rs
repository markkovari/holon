//! The node agent: how one `comp-host` joins a lattice and does as it is told.
//!
//! Three jobs, and no more:
//!
//! * publish what is running here, as a full snapshot, on a timer;
//! * take `start`/`stop` commands and make them true; and
//! * fetch artifacts by digest.
//!
//! There is no scheduler here and no opinion about placement. The reconciler
//! decides; this obeys and reports. That split is the whole reason a node can be
//! added by installing a binary and joining a tailnet.
//!
//! **It keeps serving when the control plane is gone.** The instance table is
//! persisted on every accepted command and restored *before* NATS is contacted, so
//! a node that reboots during an outage comes back running what it was running. An
//! unreachable reconciler is not an instruction to stop.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use comp_lattice::{Artifacts, CommandBus, Inventory};
use serde::{Deserialize, Serialize};

use crate::tenant::{instance_id, Limits, StartCommand};
use crate::{Instance, Instances, Routes};

/// The capabilities this host can actually grant.
///
/// The successor to the renderer's `OPERATOR_BOUND`, and still the
/// highest-consequence list in the platform — except that it is now enforced by
/// the linker rather than by a renderer's omission. A component importing anything
/// not on it does not start.
/// VERSIONLESS on purpose, and this must stay in step with `manifest.rs`'s
/// `HostIface::family`. A node advertises a concrete version and a component
/// imports one; requiring the two strings to be equal made every deployment
/// permanently unschedulable the first time it was tried live, because the host
/// said `wasi:keyvalue/store@0.2.0-draft` and the manifest asked for
/// `wasi:keyvalue/store`.
// ponytail: family match; tighten to semver when two incompatible versions of one
// interface actually have to coexist on a node.
pub const HOST_IFACES: &[&str] = &[
    "wasi:http/incoming-handler",
    "wasi:http/outgoing-handler",
    "wasi:keyvalue/store",
    "wasi:keyvalue/atomics",
    "wasi:keyvalue/batch",
    "wasi:config/store",
];

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RunningInstance {
    pub tenant: String,
    pub app: String,
    pub component: String,
    pub digest: String,
    pub count: u32,
    /// The Host header this instance answers to, so an ingress can build its
    /// routing table from inventory alone.
    pub ingress_host: Option<String>,
}

#[derive(Serialize)]
struct Snapshot<'a> {
    node: &'a str,
    labels: &'a BTreeMap<String, String>,
    host_ifaces: &'a [&'a str],
    kv_shared: bool,
    address: &'a str,
    capacity: Capacity,
    instances: Vec<RunningInstance>,
}

#[derive(Serialize)]
struct Capacity {
    cpus: usize,
    instances: usize,
}

/// What a node needs to obey a command. Everything here is process-lifetime.
pub struct Agent {
    pub node: String,
    pub labels: BTreeMap<String, String>,
    pub lattice: String,
    pub engine: Arc<wasmtime::Engine>,
    pub kv: crate::Kv,
    pub cache_backing: crate::CacheBacking,
    /// Where a granted secret is fetched from (ADR-0051). Carried on the agent so
    /// the wRPC-served path and the HTTP path build identical stores.
    pub platform_url: String,
    pub instances: Instances,
    /// Compiled artifacts, by digest.
    ///
    /// A `Component` is immutable machine code and is internally reference-counted,
    /// so every app running the same digest can share ONE copy — per-instance state
    /// lives in the Store, not here. Without this the host loaded the module once
    /// per instance: 16 apps sharing one component cost 16 copies and 3.17 MiB each,
    /// which is the marketplace case (many tenants, one popular component) paying
    /// for itself N times.
    pub compiled: Arc<std::sync::RwLock<std::collections::HashMap<String, wasmtime::component::Component>>>,
    pub routes: Routes,
    pub limits: Limits,
    /// The node's NATS connection, for building wRPC clients. `None` off a lattice.
    pub nats: Option<Arc<async_nats::Client>>,
    pub state_dir: PathBuf,
    pub heartbeat_secs: u64,
    /// Where this node can be reached by an ingress, `host:port`. A node bound to
    /// `0.0.0.0` knows its port and not its address, so this is told to it.
    pub address: String,
    /// Can every replica of an app see this node's store, wherever it runs?
    ///
    /// Advertised so the reconciler can refuse to spread a stateful app across
    /// nodes where it would silently diverge. `--kv sqlite`/`memory` say false.
    pub kv_shared: bool,
}

impl Agent {
    fn artifact_dir(&self) -> PathBuf {
        self.state_dir.join("artifacts")
    }

    fn ledger(&self) -> PathBuf {
        self.state_dir.join("instances.json")
    }

    /// Where compiled artifacts live, keyed by the digest of the wasm they came from.
    fn cache_dir(&self) -> PathBuf {
        self.state_dir.join("cache")
    }

    /// Everything running here, for the inventory and for the ledger.
    fn snapshot(&self) -> Vec<RunningInstance> {
        // Reverse the route table once rather than per instance: a node holds few
        // instances, but this runs on every heartbeat.
        let routes: BTreeMap<String, String> = self
            .routes
            .read()
            .unwrap()
            .iter()
            .map(|(host, id)| (id.clone(), host.clone()))
            .collect();
        self.instances
            .read()
            .unwrap()
            .iter()
            .map(|(id, i)| RunningInstance {
                tenant: i.scope.tenant.clone(),
                app: i.scope.app.clone(),
                component: i.scope.component.clone(),
                digest: i.scope.digest.clone(),
                count: i.count,
                ingress_host: routes.get(id).cloned(),
            })
            .collect()
    }

    /// Persist what we were told to run, so a reboot is not a data-loss event for
    /// the fleet's desired state. Atomic rename: a half-written ledger read on the
    /// next boot would start a subset and look like a partial outage.
    fn persist(&self, commands: &BTreeMap<String, StartCommand>) {
        let tmp = self.ledger().with_extension("json.tmp");
        let Ok(bytes) = serde_json::to_vec_pretty(commands) else { return };
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, self.ledger());
        }
    }

    fn load_ledger(&self) -> BTreeMap<String, StartCommand> {
        std::fs::read(self.ledger())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
}

/// The commands this node has accepted, keyed by instance id. Kept beside the live
/// table because a `StartCommand` is what has to be replayed on boot — the
/// compiled instance cannot be.
type Ledger = Arc<std::sync::Mutex<BTreeMap<String, StartCommand>>>;

/// The fabric this node is joined to. Three traits, one implementation today —
/// nothing below this line names a broker.
pub struct Fabric {
    pub inventory: Arc<dyn Inventory>,
    pub commands: Arc<dyn CommandBus>,
    pub artifacts: Arc<dyn Artifacts>,
}

pub async fn run(agent: Arc<Agent>, fabric: Fabric) -> Result<()> {
    std::fs::create_dir_all(agent.artifact_dir())
        .with_context(|| format!("creating {}", agent.artifact_dir().display()))?;

    let ledger: Ledger = Arc::new(std::sync::Mutex::new(agent.load_ledger()));

    // Restore BEFORE touching the network. This is the property that replaces the
    // operator: a node that reboots while the control plane is down comes back
    // serving, from its own disk, with no help from anyone.
    {
        let saved = ledger.lock().unwrap().clone();
        if !saved.is_empty() {
            eprintln!("comp-host: restoring {} instance(s) from the ledger", saved.len());
        }
        for (id, cmd) in saved {
            if let Err(e) = start(&agent, cmd, None).await {
                // A restore failure must not stop the others: one unreadable
                // artifact is not a reason to bring up nothing.
                eprintln!("comp-host: could not restore {id}: {e:#}");
            }
        }
    }

    let Fabric { inventory, commands, artifacts } = fabric;

    // Heartbeat. Separate task so a slow command cannot make this node look dead
    // and get its work rescheduled underneath it.
    {
        let agent = agent.clone();
        let inventory = inventory.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(agent.heartbeat_secs));
            loop {
                tick.tick().await;
                let inv = Snapshot {
                    node: &agent.node,
                    labels: &agent.labels,
                    host_ifaces: HOST_IFACES,
                    kv_shared: agent.kv_shared,
                    address: &agent.address,
                    capacity: Capacity {
                        cpus: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
                        instances: agent.instances.read().unwrap().len(),
                    },
                    instances: agent.snapshot(),
                };
                match serde_json::to_vec(&inv) {
                    Ok(bytes) => {
                        // Three missed beats. A flaky tailnet gets chances; a dead
                        // node does not linger long enough to hold a replica hostage.
                        let ttl = Duration::from_secs(agent.heartbeat_secs * 3);
                        if let Err(e) = inventory.publish(&agent.node, bytes, ttl).await {
                            eprintln!("comp-host: heartbeat failed: {e:#}");
                        }
                    }
                    Err(e) => eprintln!("comp-host: could not serialise inventory: {e}"),
                }
            }
        });
    }

    let mut inbox = commands.serve(&agent.node).await.context("taking delivery of commands")?;
    eprintln!("comp-host: joined lattice {} as node {}", agent.lattice, agent.node);

    while let Some(cmd) = inbox.recv().await {
        let result = handle(&agent, artifacts.as_ref(), &ledger, &cmd.verb, &cmd.payload).await;

        // Acked only after the instance is built, so "started" means "will serve"
        // rather than "is downloading".
        let body = match &result {
            Ok(note) => serde_json::json!({ "ok": true, "note": note }),
            Err(e) => {
                eprintln!("comp-host: {} failed: {e:#}", cmd.verb);
                serde_json::json!({ "error": format!("{e:#}") })
            }
        };
        let _ = cmd.reply.send(body.to_string().into_bytes());
    }
    bail!("the command stream ended — the fabric closed the connection")
}

async fn handle(
    agent: &Arc<Agent>,
    artifacts: &dyn Artifacts,
    ledger: &Ledger,
    verb: &str,
    payload: &[u8],
) -> Result<String> {
    match verb {
        "start" => {
            let cmd: StartCommand =
                serde_json::from_slice(payload).context("unreadable start command")?;
            let id = instance_id(&cmd.tenant, &cmd.app, &cmd.component);
            start(agent, cmd.clone(), Some(artifacts)).await?;
            let saved = {
                let mut l = ledger.lock().unwrap();
                l.insert(id.clone(), cmd);
                l.clone()
            };
            agent.persist(&saved);
            Ok(format!("started {id}"))
        }
        "stop" => {
            #[derive(Deserialize)]
            struct Stop {
                tenant: String,
                app: String,
                component: String,
            }
            let s: Stop = serde_json::from_slice(payload).context("unreadable stop command")?;
            let id = instance_id(&s.tenant, &s.app, &s.component);
            // Shrink by the delta; remove only when nothing is left. A stop of 1
            // out of 3 must not take the whole instance down.
            // Stop means gone. Shrinking to a smaller non-zero count is a `start`
            // with a lower absolute count, so there is only one code path that
            // changes a replica count and only one that removes an instance.
            let removed = agent.instances.write().unwrap().remove(&id);
            if let Some(gone) = &removed {
                agent.routes.write().unwrap().retain(|_, v| *v != id);
                // Drop the shared module when the LAST instance on that digest goes.
                // Holding machine code for something nothing runs is exactly the idle
                // cost this cache exists to reduce, and the .cwasm stays on disk, so
                // coming back costs the 0.3ms load rather than a recompile.
                let digest = gone.scope.digest.clone();
                let still_used = agent
                    .instances
                    .read()
                    .unwrap()
                    .values()
                    .any(|i| i.scope.digest == digest);
                if !still_used {
                    agent.compiled.write().unwrap().remove(&digest);
                }
            }
            let saved = {
                let mut l = ledger.lock().unwrap();
                l.remove(&id);
                l.clone()
            };
            agent.persist(&saved);
            Ok(if removed.is_some() {
                format!("stopped {id}")
            } else {
                // Not an error: the reconciler re-derives from inventory, so
                // stopping something already gone is a converged no-op.
                format!("{id} was not running")
            })
        }
        "drain" => {
            let ids: Vec<String> = agent.instances.read().unwrap().keys().cloned().collect();
            agent.instances.write().unwrap().clear();
            agent.routes.write().unwrap().clear();
            // The ledger is deliberately NOT cleared: a drain is an operator asking
            // this node to shed load now, not a decision that these apps should
            // never come back. The reconciler will place them elsewhere.
            Ok(format!("drained {} instance(s)", ids.len()))
        }
        other => bail!("unknown command {other:?}"),
    }
}

/// Build one instance and put it in the table.
///
/// `artifacts` is `None` during a ledger restore, where only the local cache may be
/// used — the whole point of that path is that it works with no network.
async fn start(
    agent: &Arc<Agent>,
    cmd: StartCommand,
    artifacts: Option<&dyn Artifacts>,
) -> Result<()> {
    let id = instance_id(&cmd.tenant, &cmd.app, &cmd.component);
    let ingress_host = cmd.ingress_host.clone();
    // A start command says how many this node should hold — an absolute count, not
    // a delta. Re-sending it is therefore a no-op, which matters because the
    // reconciler re-derives faster than this node heartbeats and will legitimately
    // repeat itself. (A delta here put six replicas of a two-replica app across two
    // machines on the first cross-machine run.)
    //
    // The clone-then-drop dance is not ceremony either. `if let Some(x) =
    // lock.read()…` holds the read guard for the whole block, so taking the write
    // lock inside it deadlocks — and because the heartbeat also reads this table,
    // the node then stops publishing inventory and gets its work rescheduled out
    // from under it. Also measured, not theorised.
    let resized = {
        let table = agent.instances.read().unwrap();
        table
            .get(&id)
            .filter(|e| e.scope.digest == cmd.digest && e.count != cmd.count.max(1))
            .map(|e| {
                Arc::new(Instance {
                    scope: e.scope.clone(),
                    pre: e.pre.clone(),
                    remotes: e.remotes.clone(),
                    count: cmd.count.max(1),
                })
            })
    };
    if let Some(resized) = resized {
        let n = resized.count;
        agent.instances.write().unwrap().insert(id.clone(), resized);
        eprintln!("comp-host: {id} now holds {n} replica(s)");
        return Ok(());
    }
    // Already exactly as asked: say so and touch nothing.
    if agent.instances.read().unwrap().get(&id).is_some_and(|e| e.scope.digest == cmd.digest) {
        return Ok(());
    }

    // Omission fails closed. A component importing something this host cannot
    // grant is refused HERE, at start, rather than trapping on its first request
    // in front of a user.
    for need in &cmd.host_needs {
        if !HOST_IFACES.contains(&need.as_str()) {
            bail!("{id} imports {need}, which this host cannot grant");
        }
    }

    // Phase timings, because "cold start" is three different costs and only one of
    // them is worth optimising. Reported on the start line so the number comes from
    // the node doing the work rather than from a stopwatch on the other side of a
    // NATS round trip.
    let t0 = std::time::Instant::now();
    let path = fetch_artifact(agent, &cmd.digest, artifacts).await?;
    let t_fetch = t0.elapsed();
    let count = cmd.count.max(1);
    let scope = Arc::new(cmd.into_scope(&agent.limits));

    // Compilation is slow and blocking; a start command must not stall the
    // heartbeat behind it.
    let engine = agent.engine.clone();
    // Compile once per artifact per node, not once per start. ADR-0037 measured a
    // 33ms cold start of which 31ms was `Component::from_file` recompiling bytes
    // this node had already compiled — on every start, every re-placement after a
    // node dies, and every reboot.
    let cache = agent.cache_dir();
    let _ = std::fs::create_dir_all(&cache);
    let cwasm = cache.join(format!("{}.cwasm", scope.digest.trim_start_matches("sha256:")));
    let engine = agent.engine.clone();
    // Already compiled on this node? Then share it. A `Component` is immutable
    // machine code and internally reference-counted, so N apps on one digest hold one
    // copy — per-instance state lives in the Store. Everything below still runs per
    // instance: the linker, the remotes and the route are what make it an instance.
    let in_memory = agent.compiled.read().unwrap().get(&scope.digest).cloned();
    let shared = in_memory.is_some();
    let (component, from_cache) = match in_memory {
        Some(hit) => (hit, true),
        None => tokio::task::spawn_blocking(move || {
        // SAFETY: `deserialize_file` trusts its input completely — it maps machine
        // code straight in. The only thing that makes that acceptable is where the
        // file comes from: written by this process, into a host-private directory,
        // named for the digest whose bytes we verified before compiling. Nothing
        // off the wire is ever deserialised, and a file that came from anywhere
        // else would be arbitrary code execution.
        if cwasm.exists() {
            match unsafe { wasmtime::component::Component::deserialize_file(&engine, &cwasm) } {
                Ok(c) => return Ok((c, true)),
                // A cache written by a different wasmtime build, or a truncated
                // write, must not be fatal — it is a cache. Drop it and compile.
                Err(e) => {
                    eprintln!("comp-host: ignoring unusable {}: {e}", cwasm.display());
                    let _ = std::fs::remove_file(&cwasm);
                }
            }
        }
        let c = wasmtime::component::Component::from_file(&engine, &path)?;
        // Write via a temp file in the same directory, then rename. Two starts of
        // the same digest can race here, and a half-written .cwasm that another
        // start deserialises is the one failure this cache must never cause.
        if let Ok(bytes) = c.serialize() {
            let tmp = cwasm.with_extension(format!("tmp.{}", std::process::id()));
            if std::fs::write(&tmp, &bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &cwasm);
            }
        }
            Ok::<_, anyhow::Error>((c, false))
        })
        .await
        .context("compile task")?
        .map_err(|e| anyhow::anyhow!("compiling the artifact for {id}: {e}"))?,
    };
    let t_compile = t0.elapsed() - t_fetch;

    // EVERY link becomes a wRPC client, including one whose target is running in
    // this very process.
    //
    // That reads wrong and is not. There is no in-process path between two
    // SEPARATELY STARTED components: the host satisfies an import from a host
    // capability or from wRPC, and nothing else. Skipping a local target leaves its
    // import unbound and the instance fails to start with "a matching
    // implementation was not found in the linker" — measured, by trying it.
    //
    // Components that link in-process do so because `wac` fused them at build time,
    // which is a different mechanism entirely (ADR-0005's two strategies).
    //
    // It also costs almost nothing: co-located over the loopback bus measured 2,795
    // rps against 2,788 for the same graph, i.e. inside noise. A local short-circuit
    // would be an optimisation worth roughly 0.3%, and would first require building
    // instance-to-instance linking that does not exist.
    let remotes = match (&agent.nats, scope.links.is_empty()) {
        (Some(nats), false) => {
            crate::rpc::remote_clients(nats.clone(), &agent.lattice, &scope.links).await?
        }
        _ => Default::default(),
    };
    if !remotes.is_empty() {
        eprintln!("comp-host: {id} links {} interface(s) over wrpc", remotes.len());
    }

    // Remembered by digest, so the next app on this component shares this copy.
    // Inserted before the linker because everything below is per-instance and this
    // is the only part that is not.
    if !shared {
        agent.compiled.write().unwrap().insert(scope.digest.clone(), component.clone());
    }

    let mut linker = crate::build_linker(&agent.engine)?;
    // Bind the remote half BEFORE pre-instantiating: an import with no host impl
    // and no link is what makes `instantiate_pre` fail, and failing there is how
    // omission stays fail-closed (ADR-0013).
    if !remotes.is_empty() {
        let n = crate::rpc::link_remote_imports(&agent.engine, &mut linker, &component, &remotes)?;
        eprintln!("comp-host: {id} bound {n} interface(s) over wrpc");
    }
    let ipre = linker.instantiate_pre(&component)?;
    let t_link = t0.elapsed() - t_fetch - t_compile;
    // A component that exports `wasi:http/incoming-handler` gets a door; one that
    // does not is a plug, reachable over the bus only. Both are legal instances —
    // before the serve side existed, the second could not start at all.
    let pre = wasmtime_wasi_http::p2::bindings::ProxyPre::new(ipre.clone()).ok();

    // Serve this instance's exports so a remote import has something to call. On the
    // instance's own subject, in a queue group named for it, so N replicas share the
    // work and a departing one needs no deregistration.
    if let Some(nats) = &agent.nats {
        let exports = crate::rpc::exported_functions(&agent.engine, &component);
        if !exports.is_empty() {
            let serve_client =
                crate::rpc::client(nats.clone(), &agent.lattice, &id, Some(&id)).await?;
            let (kv, cache, sc, rem) =
                (agent.kv.clone(), agent.cache_backing.clone(), scope.clone(), remotes.clone());
            let engine = agent.engine.clone();
            let platform = agent.platform_url.clone();
            let n = crate::rpc::serve_exports_over(
                &agent.engine,
                &component,
                ipre,
                &serve_client,
                move || {
                    crate::store_for(
                        &engine,
                        sc.clone(),
                        kv.clone(),
                        cache.clone(),
                        rem.clone(),
                        platform.clone(),
                    )
                },
            )
            .await?;
            eprintln!("comp-host: {id} serves {n} function(s) to the lattice");
        }
    }

    agent
        .instances
        .write()
        .unwrap()
        .insert(id.clone(), Arc::new(Instance { scope: scope.clone(), pre, remotes, count }));
    if let Some(host) = ingress_host {
        agent.routes.write().unwrap().insert(host.to_ascii_lowercase(), id.clone());
    }
    eprintln!(
        "comp-host: started {id} ({}) in {} us (fetch {} us, {} {} us, link {} us)",
        scope.digest,
        t0.elapsed().as_micros(),
        t_fetch.as_micros(),
        if shared { "shared" } else if from_cache { "cache-load" } else { "compile" },
        t_compile.as_micros(),
        t_link.as_micros(),
    );
    Ok(())
}

/// Local cache first, object store second, and the digest is checked either way.
///
/// The object store is not a trust boundary — the digest is. Anything that does not
/// hash to the name it was fetched under is discarded rather than compiled.
async fn fetch_artifact(
    agent: &Arc<Agent>,
    digest: &str,
    artifacts: Option<&dyn Artifacts>,
) -> Result<PathBuf> {
    let short = digest.trim_start_matches("sha256:");
    if short.is_empty() || !short.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("{digest:?} is not a sha256 digest");
    }
    let path = agent.artifact_dir().join(format!("{short}.wasm"));
    if path.exists() {
        return Ok(path);
    }
    let Some(store) = artifacts else {
        bail!("{digest} is not in the local cache and there is no store to fetch it from");
    };

    let bytes = store
        .get(digest)
        .await
        .with_context(|| format!("fetching {digest} from the artifact store"))?;

    let got = sha256_hex(&bytes);
    if got != short {
        bail!("artifact {digest} hashes to sha256:{got} — refusing to compile it");
    }
    write_atomic(&path, &bytes)?;
    Ok(path)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("wasm.tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_that_is_not_a_digest_is_refused() {
        // The artifact path is a filename built from this string. A digest with a
        // slash in it would be a directory traversal into whatever the host can
        // write, so it is validated as hex before it is ever joined to a path.
        for bad in ["", "sha256:", "sha256:../../etc/passwd", "sha256:zz", "latest", "sha256:ab/cd"]
        {
            let short = bad.trim_start_matches("sha256:");
            let ok = !short.is_empty() && short.chars().all(|c| c.is_ascii_hexdigit());
            assert!(!ok, "{bad:?} must not pass the digest check");
        }
        let good = "sha256:deadbeefcafe";
        let short = good.trim_start_matches("sha256:");
        assert!(!short.is_empty() && short.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn the_host_interface_list_is_what_the_linker_actually_provides() {
        // This list is the successor to the renderer's OPERATOR_BOUND and it is
        // still the highest-consequence list in the platform: anything on it is
        // something a tenant's component may import. Adding a line here without
        // adding it to `build_linker` promises something that then fails at start.
        assert!(HOST_IFACES.iter().all(|i| i.contains(':') && i.contains('/')));
        // Versionless, matching what a manifest stamps into `host_needs`. If these
        // ever carry a version again, every deployment becomes unschedulable.
        assert!(!HOST_IFACES.iter().any(|i| i.contains('@')));
        // wasmcloud:messaging stays off it. Raw subject publish is the one
        // capability that reaches around the host's naming, which would break the
        // boundary for every NATS-backed thing at once.
        assert!(!HOST_IFACES.iter().any(|i| i.starts_with("wasmcloud:messaging")));
    }

    #[test]
    fn sha256_matches_the_reconcilers() {
        // Both sides content-address independently; if these ever disagree, every
        // artifact fetch fails its integrity check for a reason nobody would guess.
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
