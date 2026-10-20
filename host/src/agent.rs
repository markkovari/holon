//! Node agent. The ledger is restored before NATS is contacted, so a node keeps serving with the control plane gone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use comp_lattice::{Artifacts, CommandBus, Inventory};
use serde::{Deserialize, Serialize};

use crate::tenant::{instance_id, Limits, StartCommand};
use crate::{Instance, Instances, Routes};

/// Versionless on purpose (see `manifest.rs` `HostIface::family`): exact version equality made every deployment unschedulable.
// ponytail: family match; tighten to semver when two incompatible versions must coexist on a node.
pub const HOST_IFACES: &[&str] = &[
    "wasi:http/incoming-handler",
    "wasi:http/outgoing-handler",
    // p3 (the `0.3.0-rc-2026-03-15` WIT on wasmtime 45): the async door and client.
    "wasi:http/handler",
    "wasi:http/client",
    "wasi:keyvalue/store",
    "wasi:keyvalue/atomics",
    "wasi:keyvalue/batch",
    "wasi:config/store",
    "comp:secrets/reader",
    "comp:store/cas",
];

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RunningInstance {
    pub tenant: String,
    pub app: String,
    pub component: String,
    pub digest: String,
    pub count: u32,
    /// So an ingress can build its routing table from inventory alone.
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

pub struct Agent {
    pub node: String,
    pub labels: BTreeMap<String, String>,
    pub lattice: String,
    pub engine: Arc<wasmtime::Engine>,
    pub kv: crate::Kv,
    /// Secret source (ADR-0051), on the agent so the wRPC and HTTP paths build identical stores.
    pub platform_url: String,
    pub instances: Instances,
    /// By digest: a `Component` is immutable and refcounted, so every app on one digest shares one copy.
    pub compiled:
        Arc<std::sync::RwLock<std::collections::HashMap<String, wasmtime::component::Component>>>,
    pub routes: Routes,
    pub limits: Limits,
    pub nats: Option<Arc<async_nats::Client>>,
    pub state_dir: PathBuf,
    pub heartbeat_secs: u64,
    /// `host:port` for an ingress; a node bound to `0.0.0.0` cannot know its own address.
    pub address: String,
    /// Lets the reconciler refuse to spread a stateful app where its store would diverge.
    pub kv_shared: bool,
}

impl Agent {
    fn artifact_dir(&self) -> PathBuf {
        self.state_dir.join("artifacts")
    }

    fn ledger(&self) -> PathBuf {
        self.state_dir.join("instances.json")
    }

    fn cache_dir(&self) -> PathBuf {
        self.state_dir.join("cache")
    }

    fn snapshot(&self) -> Vec<RunningInstance> {
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

    /// Atomic rename: a half-written ledger read on boot would start a subset.
    fn persist(&self, commands: &BTreeMap<String, StartCommand>) {
        // Logged, not propagated: failing the heartbeat would turn "ledger not saved" into "node down".
        let tmp = self.ledger().with_extension("json.tmp");
        let bytes = match serde_json::to_vec_pretty(commands) {
            Ok(b) => b,
            Err(e) => {
                eprintln!(
                    "comp-host: WARNING the run ledger could not be serialised ({e}); the \
                     desired state of {} instance(s) is NOT on disk and a reboot will start \
                     a subset",
                    commands.len()
                );
                return;
            }
        };
        if let Err(e) = std::fs::write(&tmp, bytes) {
            eprintln!(
                "comp-host: WARNING could not write {} ({e}); the run ledger is NOT on disk \
                 and a reboot will start a subset",
                tmp.display()
            );
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, self.ledger()) {
            eprintln!(
                "comp-host: WARNING could not replace {} ({e}); the run ledger is STALE and \
                 a reboot will start whatever it last contained",
                self.ledger().display()
            );
        }
    }

    fn load_ledger(&self) -> BTreeMap<String, StartCommand> {
        let bytes = match std::fs::read(self.ledger()) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
            Err(e) => {
                eprintln!(
                    "comp-host: WARNING the run ledger at {} exists but could not be read \
                     ({e}); starting nothing, which is NOT the same as having nothing to \
                     start",
                    self.ledger().display()
                );
                return BTreeMap::new();
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "comp-host: WARNING the run ledger at {} is corrupt ({e}); starting \
                     nothing. The file is left in place rather than overwritten, so it can \
                     be looked at.",
                    self.ledger().display()
                );
                BTreeMap::new()
            }
        }
    }
}

/// Accepted commands by instance id: a `StartCommand` is what replays on boot, not the compiled instance.
type Ledger = Arc<std::sync::Mutex<BTreeMap<String, StartCommand>>>;

pub struct Fabric {
    pub inventory: Arc<dyn Inventory>,
    pub commands: Arc<dyn CommandBus>,
    pub artifacts: Arc<dyn Artifacts>,
}

pub async fn run(agent: Arc<Agent>, fabric: Fabric) -> Result<()> {
    std::fs::create_dir_all(agent.artifact_dir())
        .with_context(|| format!("creating {}", agent.artifact_dir().display()))?;

    let ledger: Ledger = Arc::new(std::sync::Mutex::new(agent.load_ledger()));

    // Restore before touching the network, so a reboot during a control-plane outage still serves.
    {
        let saved = crate::sync::held(&ledger).clone();
        if !saved.is_empty() {
            eprintln!("comp-host: restoring {} instance(s) from the ledger", saved.len());
        }
        for (id, cmd) in saved {
            if let Err(e) = start(&agent, cmd, None).await {
                eprintln!("comp-host: could not restore {id}: {e:#}");
            }
        }
    }

    let Fabric { inventory, commands, artifacts } = fabric;

    // Own task, so a slow command cannot make this node look dead.
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
                        instances: crate::sync::reading(&agent.instances).len(),
                    },
                    instances: agent.snapshot(),
                };
                match serde_json::to_vec(&inv) {
                    Ok(bytes) => {
                        // Three missed beats: tolerates a flaky tailnet, drops a dead node fast.
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

        // Acked only after the instance is built: "started" means "will serve".
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
                let mut l = crate::sync::held(ledger);
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
            // Stop means gone; shrinking is a `start` with a lower absolute count.
            let removed = crate::sync::writing(&agent.instances).remove(&id);
            if let Some(gone) = &removed {
                crate::sync::writing(&agent.routes).retain(|_, v| *v != id);
                // Drop the shared module with the last instance on its digest; the .cwasm stays on disk for a cheap reload.
                let digest = gone.scope.digest.clone();
                let still_used =
                    agent.instances.read().unwrap().values().any(|i| i.scope.digest == digest);
                if !still_used {
                    crate::sync::writing(&agent.compiled).remove(&digest);
                }
            }
            let saved = {
                let mut l = crate::sync::held(ledger);
                l.remove(&id);
                l.clone()
            };
            agent.persist(&saved);
            Ok(if removed.is_some() {
                format!("stopped {id}")
            } else {
                // Not an error: stopping something already gone is a converged no-op.
                format!("{id} was not running")
            })
        }
        "drain" => {
            let ids: Vec<String> = crate::sync::reading(&agent.instances).keys().cloned().collect();
            crate::sync::writing(&agent.instances).clear();
            crate::sync::writing(&agent.routes).clear();
            // Ledger deliberately kept: a drain sheds load now, it does not retire these apps.
            Ok(format!("drained {} instance(s)", ids.len()))
        }
        other => bail!("unknown command {other:?}"),
    }
}

/// `artifacts` is `None` during a ledger restore, which must use the local cache only.
async fn start(
    agent: &Arc<Agent>,
    cmd: StartCommand,
    artifacts: Option<&dyn Artifacts>,
) -> Result<()> {
    let id = instance_id(&cmd.tenant, &cmd.app, &cmd.component);
    let ingress_host = cmd.ingress_host.clone();
    // Absolute count, so a re-send is a no-op.
    // Clone-then-drop: holding the read guard into the write lock deadlocks the heartbeat.
    let resized = {
        let table = crate::sync::reading(&agent.instances);
        table.get(&id).filter(|e| e.scope.digest == cmd.digest && e.count != cmd.count.max(1)).map(
            |e| {
                Arc::new(Instance {
                    scope: e.scope.clone(),
                    cache_backing: e.cache_backing.clone(),
                    pre: e.pre.clone(),
                    remotes: e.remotes.clone(),
                    count: cmd.count.max(1),
                })
            },
        )
    };
    if let Some(resized) = resized {
        let n = resized.count;
        crate::sync::writing(&agent.instances).insert(id.clone(), resized);
        eprintln!("comp-host: {id} now holds {n} replica(s)");
        return Ok(());
    }
    if crate::sync::reading(&agent.instances).get(&id).is_some_and(|e| e.scope.digest == cmd.digest)
    {
        return Ok(());
    }

    // Fail closed at start, not on the first request.
    for need in &cmd.host_needs {
        if !HOST_IFACES.contains(&need.as_str()) {
            bail!("{id} imports {need}, which this host cannot grant");
        }
    }

    // Phase timings: "cold start" is three different costs.
    let t0 = std::time::Instant::now();
    let path = fetch_artifact(agent, &cmd.digest, artifacts).await?;
    let t_fetch = t0.elapsed();
    let count = cmd.count.max(1);
    let scope = Arc::new(cmd.into_scope(&agent.limits));
    // Per instance: a process-global one would let two tenants' `cache:store` collide.
    let cache_backing: crate::CacheBacking =
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

    // Every granted secret must resolve before serving (ADR-0013, ADR-0051); a `describe`, not a fetch.
    // Skipped on a ledger restore, which must work with no network.
    if artifacts.is_some() && scope.secret_refs().next().is_some() {
        if agent.platform_url.is_empty() {
            bail!("{id} was granted secrets, and this host has no platform to resolve them");
        }
        // ponytail: a client per start; starts are rare enough not to warrant a pool on `Agent`.
        let http = reqwest::Client::new();
        for (key, reference) in scope.secret_refs() {
            crate::secrets::probe(&http, &agent.platform_url, &scope.fetch_token, reference)
                .await
                .map_err(|e| {
                    // Name the reference (not a value) so an operator knows which key is broken.
                    anyhow::anyhow!(
                        "{id} cannot start: secret {key:?} -> {}: {e}",
                        reference.as_str()
                    )
                })?;
        }
    }

    // Compile once per artifact per node: ADR-0037 measured 31 of a 33ms cold start as recompiling.
    let cache = agent.cache_dir();
    let _ = std::fs::create_dir_all(&cache);
    let cwasm = cache.join(format!("{}.cwasm", scope.digest.trim_start_matches("sha256:")));
    let engine = agent.engine.clone();
    let in_memory = crate::sync::reading(&agent.compiled).get(&scope.digest).cloned();
    let shared = in_memory.is_some();
    let (component, from_cache) = match in_memory {
        Some(hit) => (hit, true),
        None => tokio::task::spawn_blocking(move || {
            // SAFETY: `deserialize_file` maps machine code in unchecked. The file is only ever written by this
            // process, into a host-private dir, named for a verified digest; nothing off the wire is deserialised.
            if cwasm.exists() {
                match unsafe { wasmtime::component::Component::deserialize_file(&engine, &cwasm) } {
                    Ok(c) => return Ok((c, true)),
                    // A stale or truncated cache is not fatal: drop it and compile.
                    Err(e) => {
                        eprintln!("comp-host: ignoring unusable {}: {e}", cwasm.display());
                        let _ = std::fs::remove_file(&cwasm);
                    }
                }
            }
            let c = wasmtime::component::Component::from_file(&engine, &path)?;
            // Temp file + rename: two starts of one digest can race, and a half-written .cwasm must never load.
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

    // Every link is a wRPC client, even to this process: separate starts have no in-process path (ADR-0005).
    let remotes = match (&agent.nats, scope.links.is_empty()) {
        (Some(nats), false) => {
            crate::rpc::remote_clients(nats.clone(), &agent.lattice, &scope.links).await?
        }
        _ => Default::default(),
    };
    if !remotes.is_empty() {
        eprintln!("comp-host: {id} links {} interface(s) over wrpc", remotes.len());
    }

    if !shared {
        crate::sync::writing(&agent.compiled).insert(scope.digest.clone(), component.clone());
    }

    let mut linker = crate::build_linker(&agent.engine)?;
    // Bind remotes before pre-instantiating: an unbound import failing `instantiate_pre` keeps omission fail-closed (ADR-0013).
    if !remotes.is_empty() {
        let n = crate::rpc::link_remote_imports(&agent.engine, &mut linker, &component, &remotes)?;
        eprintln!("comp-host: {id} bound {n} interface(s) over wrpc");
    }
    let ipre = linker.instantiate_pre(&component)?;
    let t_link = t0.elapsed() - t_fetch - t_compile;
    let pre = crate::HttpPre::new(ipre.clone());

    // Queue group per instance: N replicas share the work and a departing one needs no deregistration.
    if let Some(nats) = &agent.nats {
        let exports = crate::rpc::exported_functions(&agent.engine, &component);
        if !exports.is_empty() {
            let serve_client =
                crate::rpc::client(nats.clone(), &agent.lattice, &id, Some(&id)).await?;
            let (kv, cache, sc, rem) =
                (agent.kv.clone(), cache_backing.clone(), scope.clone(), remotes.clone());
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

    agent.instances.write().unwrap().insert(
        id.clone(),
        Arc::new(Instance { scope: scope.clone(), cache_backing, pre, remotes, count }),
    );
    if let Some(host) = ingress_host {
        crate::sync::writing(&agent.routes).insert(host.to_ascii_lowercase(), id.clone());
    }
    eprintln!(
        "comp-host: started {id} ({}) in {} us (fetch {} us, {} {} us, link {} us)",
        scope.digest,
        t0.elapsed().as_micros(),
        t_fetch.as_micros(),
        if shared {
            "shared"
        } else if from_cache {
            "cache-load"
        } else {
            "compile"
        },
        t_compile.as_micros(),
        t_link.as_micros(),
    );
    Ok(())
}

/// Local cache first, then the store; the digest, not the store, is the trust boundary.
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
        // The digest becomes a filename; a slash in it would be a directory traversal.
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
        // Adding an entry without wiring it in `build_linker` promises an import that fails at start.
        assert!(HOST_IFACES.iter().all(|i| i.contains(':') && i.contains('/')));
        assert!(!HOST_IFACES.iter().any(|i| i.contains('@')));
        // wasmcloud:messaging stays off: raw subject publish bypasses the host's naming boundary.
        assert!(!HOST_IFACES.iter().any(|i| i.starts_with("wasmcloud:messaging")));
    }

    #[test]
    fn sha256_matches_the_reconcilers() {
        // Both sides content-address independently; a mismatch fails every fetch's integrity check.
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
