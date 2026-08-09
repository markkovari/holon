//! The diff: desired manifests vs observed lattice, in, commands out.
//!
//! Pure. No I/O, no clock, no network — which is the point. This is the function
//! that decides what runs where, so it is the one that has to be tested to
//! destruction rather than watched in production. `render.rs` earned its 17 tests
//! for the same reason and this replaces it.
//!
//! **It assumes `desired` is complete.** A partial list reads as "these apps were
//! deleted" and stops them. The caller's job is to never call this with the result
//! of a failed poll — see the `continue` in `reapply_loop`, which is load-bearing
//! for exactly this reason and predates the lattice.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

// ---- desired state ---------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    /// One artifact, `wac plug`-composed at save. Only the root exists at runtime.
    Fused,
    /// N components, runtime-linked by the host from a link table.
    Linked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// `replicas` distributed over the eligible nodes.
    #[default]
    Spread,
    /// One per eligible node; `replicas` is ignored.
    Daemon,
    /// Exactly the named nodes.
    Pinned,
}

/// How many replicas an app should have, as a function of how busy it is.
///
/// `target` is concurrent requests per replica, not requests per second: it is what
/// the ingress can actually observe (it holds an in-flight counter per backend
/// already), and it needs no clock or window. Rate needs both, and a rate averaged
/// over the wrong window is how autoscalers oscillate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scale {
    /// `0` is accepted and scales the app away — but nothing brings it BACK on a
    /// request today: with no replica placed there is no route, and the ingress
    /// answers 503. ADR-0037 measured a 33 ms start, so activation-on-request is
    /// affordable; until it exists, treat `min: 0` as "park this app", not as
    /// scale-to-zero.
    /// ponytail: activation path pending; min 1 is the usable floor.
    pub min: u32,
    pub max: u32,
    /// Concurrent requests one replica should carry before another is added.
    #[serde(default = "one")]
    pub target: u32,
}

/// Observed concurrency per ingress host, as published by the ingress.
pub type Load = BTreeMap<String, u32>;

/// How many replicas this component should have right now.
///
/// Pure, and separate from placement, so the "how many" question can be tested
/// without a fleet to put them on.
pub fn desired_replicas(c: &Component, m: &Manifest, load: &Load) -> u32 {
    let Some(scale) = &c.scale else {
        return c.replicas;
    };
    // An app nobody has reported on is not an app with no traffic — it is an app
    // the ingress has not published yet (it has just started, or it is the pass
    // before the first sample). Falling to `min` on a missing key would scale a
    // busy app to zero every time the ingress restarts.
    let Some(host) = m.ingress.as_ref().map(|i| i.host.as_str()) else {
        return c.replicas.clamp(scale.min.max(1), scale.max.max(1));
    };
    let Some(&inflight) = load.get(host) else {
        return c.replicas.clamp(scale.min, scale.max.max(scale.min));
    };
    let target = scale.target.max(1);
    // Round up: at target 10, eleven concurrent requests need two replicas, not one.
    let want = inflight.div_ceil(target);
    want.clamp(scale.min, scale.max.max(scale.min))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub nodes: Vec<String>,
    /// Matched against the labels a node advertises. Equality only.
    // ponytail: equality only; add operators when a real multiregion query needs them.
    #[serde(default)]
    pub constraints: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    /// Content address of the artifact. Identity, not a tag (ADR-0006).
    pub digest: String,
    #[serde(default = "one")]
    pub replicas: u32,
    /// Autoscaling bounds. `None` means `replicas` is a fixed count, which is what
    /// every existing manifest means and why this is an Option rather than a
    /// defaulted struct — a default of `min: 1, max: 1` would read as "autoscaling
    /// is on and pinned", and the two are different states to debug.
    #[serde(default)]
    pub scale: Option<Scale>,
    #[serde(default)]
    pub placement: Placement,
    /// Stamped from `plan.host_needs` at save, never authored. Present so this
    /// function can place with a set comparison and never needs `wit:reflect`.
    #[serde(default)]
    pub host_needs: Vec<String>,
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    /// By reference only. A value must never reach a manifest (ADR-0010).
    #[serde(default)]
    pub secrets: Vec<SecretRef>,
    /// Authorities this component may reach over `wasi:http`. Empty is deny-all.
    /// Stamped by the platform, never authored by a tenant (ADR-0008).
    #[serde(default)]
    pub egress: Vec<String>,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    pub key: String,
    #[serde(rename = "ref")]
    pub reference: String,
}

/// `composer.edge` verbatim — the same vocabulary the studio canvas emits and
/// `composer::plan` consumes. wadm's link trait names a package; this names which
/// import of which instance, which is the thing the subtype checker validates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub plug: String,
    pub socket: String,
    pub iface: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ingress {
    pub host: String,
    pub component: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub app: String,
    pub tenant: String,
    pub strategy: Strategy,
    pub components: Vec<Component>,
    #[serde(default)]
    pub links: Vec<Link>,
    #[serde(default)]
    pub ingress: Option<Ingress>,
}

impl Manifest {
    /// The component placement is computed for. Everything else in the app follows
    /// it onto the same nodes — see `place`.
    fn root(&self) -> Option<&Component> {
        match &self.ingress {
            Some(i) => self.components.iter().find(|c| c.id == i.component),
            None => self.components.first(),
        }
    }

    /// `import iface -> instance id` for one component, resolved against this app.
    /// Every entry is local because the whole graph is co-located (see `place`), so
    /// the host binds these to direct in-process calls.
    fn link_table(&self, socket: &str) -> BTreeMap<String, String> {
        self.links
            .iter()
            .filter(|l| l.socket == socket)
            .map(|l| (l.iface.clone(), format!("{}/{}/{}", self.tenant, self.app, l.plug)))
            .collect()
    }
}

// ---- observed state --------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningInstance {
    pub tenant: String,
    pub app: String,
    pub component: String,
    pub digest: String,
    #[serde(default = "one")]
    pub count: u32,
    /// The Host header this instance answers to, when it is the one serving HTTP.
    ///
    /// Advertised so an ingress can build `host -> [node]` from inventory alone and
    /// never has to ask the control plane. That keeps the data plane working while
    /// the control plane is down, which is the same property the node ledger buys.
    #[serde(default)]
    pub ingress_host: Option<String>,
}

/// One host's whole inventory, as written to the `comp-inventory` KV bucket.
///
/// A full snapshot rather than a delta: it is a few KB, it is idempotent, and it
/// means a reconciler that just started learns the world in one heartbeat cycle
/// with no replay.
// ponytail: full snapshot; switch to deltas + periodic full sync when one exceeds
// the NATS max payload, i.e. thousands of instances on one node.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NodeInventory {
    pub node: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Host capabilities this node can grant, e.g. `wasi:keyvalue/store`.
    #[serde(default)]
    pub host_ifaces: Vec<String>,
    /// Can every replica of an app see this node's store, wherever it runs?
    ///
    /// Defaults to FALSE, which matters: a node that predates this field, or one
    /// whose inventory we could not fully parse, must read as node-local. Guessing
    /// "shared" would place a stateful app across nodes that silently diverge.
    #[serde(default)]
    pub kv_shared: bool,
    /// Where this node can actually be reached, `host:port`. Not derivable from
    /// anywhere else: a node bound to `0.0.0.0` knows its port and not its address.
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub instances: Vec<RunningInstance>,
}

// ---- output ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "lowercase")]
pub enum Command {
    Start {
        node: String,
        tenant: String,
        app: String,
        component: String,
        digest: String,
        count: u32,
        config: BTreeMap<String, String>,
        secrets: Vec<SecretRef>,
        links: BTreeMap<String, String>,
        host_needs: Vec<String>,
        egress: Vec<String>,
        /// The Host header this instance answers to, when it is the one serving
        /// HTTP. `None` for a plug: it is reachable through links, not the door.
        ingress_host: Option<String>,
    },
    Stop {
        node: String,
        tenant: String,
        app: String,
        component: String,
        digest: String,
        count: u32,
    },
}

impl Command {
    pub fn node(&self) -> &str {
        match self {
            Command::Start { node, .. } | Command::Stop { node, .. } => node,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unschedulable {
    pub tenant: String,
    pub app: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub commands: Vec<Command>,
    pub unschedulable: Vec<Unschedulable>,
    /// Commands the cap dropped this pass. Never silently truncated — a dropped
    /// command reads as "converged" otherwise, and it is not.
    pub deferred: usize,
}

#[derive(Debug, Clone)]
pub struct Cfg {
    /// Consecutive passes a surplus must persist before anything is stopped.
    pub settle_passes: u32,
    /// Commands emitted per pass, so a mass event drains instead of stampeding.
    pub max_commands: usize,
}

impl Default for Cfg {
    fn default() -> Self {
        // Guesses until there is real churn to calibrate against, which is why the
        // binary exposes them as flags rather than baking them in.
        Self { settle_passes: 2, max_commands: 20 }
    }
}

/// How many consecutive passes each surplus has been observed for.
///
/// Scale *up* is not tracked: under-replicated is the bad direction and fires on
/// the first pass that sees it. Only removal waits.
#[derive(Debug, Clone, Default)]
pub struct Hysteresis {
    seen: BTreeMap<Key, u32>,
}

type Key = (String, String, String, String, String); // tenant, app, component, digest, node

// ---- the diff --------------------------------------------------------------

pub fn plan(
    desired: &[Manifest],
    observed: &[NodeInventory],
    load: &Load,
    hyst: &mut Hysteresis,
    cfg: &Cfg,
) -> Outcome {
    let mut out = Outcome::default();
    let mut want: BTreeMap<Key, (u32, &Manifest, &Component)> = BTreeMap::new();
    // Load this pass has already committed, so apps placed later in the same pass
    // see the earlier ones rather than all racing to the same node.
    let mut pending: BTreeMap<String, usize> = BTreeMap::new();

    for m in desired {
        let Some(root) = m.root() else {
            out.unschedulable.push(Unschedulable {
                tenant: m.tenant.clone(),
                app: m.app.clone(),
                reason: "manifest has no components".into(),
            });
            continue;
        };
        let want_replicas = desired_replicas(root, m, load);
        let nodes = match place(root, want_replicas, observed, &pending) {
            Ok(n) => n,
            Err(reason) => {
                out.unschedulable.push(Unschedulable {
                    tenant: m.tenant.clone(),
                    app: m.app.clone(),
                    reason,
                });
                continue;
            }
        };

        // Spreading a STATEFUL app over nodes with node-local stores gives every
        // replica its own store under the same bucket name. Nothing errors; the
        // counter just counts wrong and the failover moves the placement without
        // the data. Measured, and the reason this check exists.
        //
        // Refused rather than quietly placed, which is ADR-0013's "deny by
        // omission" instinct applied to storage: a capability nobody can partition
        // correctly is not granted at all.
        if nodes.len() > 1 && holds_state(m) {
            let local: Vec<&str> = nodes
                .iter()
                .filter(|(n, _)| {
                    !observed.iter().any(|o| o.node == *n && o.kv_shared)
                })
                .map(|(n, _)| n.as_str())
                .collect();
            if !local.is_empty() {
                out.unschedulable.push(Unschedulable {
                    tenant: m.tenant.clone(),
                    app: m.app.clone(),
                    reason: format!(
                        "spread across {} nodes but {local:?} have node-local stores — every \
                         replica would get its own store under the same bucket name and \
                         diverge in silence. Use replicas: 1, or run those nodes with a \
                         shared backend (--kv nats).",
                        nodes.len()
                    ),
                });
                continue;
            }
        }
        for (node, count) in &nodes {
            want.insert(key(m, &root.id, &root.digest, node), (*count, m, root));
            *pending.entry(node.clone()).or_default() += *count as usize;
        }
        // The rest of the graph follows the root onto the same nodes, one each.
        // That is what keeps every link local, which is what makes a link table
        // bindable to a direct in-process call.
        // ponytail: whole-graph co-location; slice two honours per-component
        // placement and pays a NATS hop for the edges it splits.
        if m.strategy == Strategy::Linked {
            for c in m.components.iter().filter(|c| c.id != root.id) {
                // A plug with placement of its OWN goes where it says, which is what
                // makes a graph span nodes: pin it to a GPU box, or a jurisdiction,
                // and the link to it becomes a wRPC call instead of an in-process
                // one. Anything without its own placement still rides along with the
                // root, because co-location is faster and should stay the default.
                let spans = !c.placement.constraints.is_empty()
                    || c.placement.mode == Mode::Pinned
                    || !c.placement.nodes.is_empty();
                let targets = if spans {
                    match place(c, c.replicas, observed, &pending) {
                        Ok(n) => n,
                        Err(reason) => {
                            out.unschedulable.push(Unschedulable {
                                tenant: m.tenant.clone(),
                                app: m.app.clone(),
                                reason: format!("`{}`: {reason}", c.id),
                            });
                            continue;
                        }
                    }
                } else {
                    nodes.clone()
                };
                for (node, _) in &targets {
                    want.insert(key(m, &c.id, &c.digest, node), (1, m, c));
                    *pending.entry(node.clone()).or_default() += 1;
                }
            }
        }
    }

    let mut have: BTreeMap<Key, u32> = BTreeMap::new();
    for inv in observed {
        for i in &inv.instances {
            *have
                .entry((
                    i.tenant.clone(),
                    i.app.clone(),
                    i.component.clone(),
                    i.digest.clone(),
                    inv.node.clone(),
                ))
                .or_default() += i.count;
        }
    }

    let (mut starts, mut stops) = (Vec::new(), Vec::new());
    let all: BTreeSet<&Key> = want.keys().chain(have.keys()).collect();
    let mut still_surplus = BTreeMap::new();

    // A command says what the world should BE, never what to change by.
    //
    // This was a delta once, and a live two-machine run killed it: the reconciler
    // reconciles faster than a host heartbeats, so it re-derived the same deficit
    // against inventory that had not caught up yet and issued the increment twice.
    // Two nodes ended up holding six replicas of a two-replica app. An absolute
    // count makes a repeated command a no-op, which is the idempotence the whole
    // "re-derive from scratch every pass" design already assumed it had.
    for k in all {
        let w = want.get(k).map(|(n, _, _)| *n).unwrap_or(0);
        let h = have.get(k).copied().unwrap_or(0);
        if w == h {
            continue;
        }

        // Scaling DOWN — including all the way to zero — waits for the surplus to
        // persist. Scaling up does not: under-replicated is the bad direction.
        if w < h {
            // Counters are never reset on emit. If the command lands the surplus is
            // gone next pass and the entry is pruned; if it did not land the
            // surplus persists and we re-emit. Re-derivation, not a state machine.
            let passes = hyst.seen.get(k).copied().unwrap_or(0) + 1;
            still_surplus.insert(k.clone(), passes);
            if passes < cfg.settle_passes {
                continue;
            }
        }

        match want.get(k) {
            Some((_, m, c)) => starts.push(Command::Start {
                node: k.4.clone(),
                tenant: m.tenant.clone(),
                app: m.app.clone(),
                component: c.id.clone(),
                digest: c.digest.clone(),
                count: w,
                config: c.config.clone(),
                secrets: c.secrets.clone(),
                links: m.link_table(&c.id),
                host_needs: c.host_needs.clone(),
                egress: c.egress.clone(),
                ingress_host: m
                    .ingress
                    .as_ref()
                    .filter(|i| i.component == c.id)
                    .map(|i| i.host.clone()),
            }),
            // Nothing wanted here at all: take it off this node.
            None => stops.push(Command::Stop {
                node: k.4.clone(),
                tenant: k.0.clone(),
                app: k.1.clone(),
                component: k.2.clone(),
                digest: k.3.clone(),
                count: h,
            }),
        }
    }
    hyst.seen = still_surplus;

    // Starts first: a digest change is a surplus of the old and a deficit of the
    // new, and ordering it this way means the replacement is up before the old one
    // goes, without a rollout state machine.
    let total = starts.len() + stops.len();
    starts.extend(stops);
    if starts.len() > cfg.max_commands {
        starts.truncate(cfg.max_commands);
    }
    out.deferred = total - starts.len();
    out.commands = starts;
    out
}

/// Does any component in this app keep state a second replica would need to see?
///
/// Derived from `host_needs` rather than declared, because `host_needs` is already
/// stamped from the real WIT surface and a separate `stateful:` flag would be a
/// second source of truth that could disagree with the imports.
// ponytail: any keyvalue import counts. An app that only ever writes node-local
// scratch is indistinguishable from one that does not, and the safe reading of an
// ambiguity here is the one that refuses.
fn holds_state(m: &Manifest) -> bool {
    m.components
        .iter()
        .flat_map(|c| c.host_needs.iter())
        .any(|h| h.starts_with("wasi:keyvalue/") || h.starts_with("wasi:blobstore/"))
}

fn key(m: &Manifest, component: &str, digest: &str, node: &str) -> Key {
    (m.tenant.clone(), m.app.clone(), component.into(), digest.into(), node.into())
}

/// Which nodes run how many, or why none can.
fn place(
    c: &Component,
    // How many to place. Passed in rather than read off `c.replicas`, because with
    // autoscaling the count is a function of load and `c` is only the policy.
    replicas: u32,
    observed: &[NodeInventory],
    // What THIS pass has already decided to put on each node.
    //
    // Without it, every app in a pass ranks against the same unchanged inventory
    // and they all choose the same node — the inventory only catches up a
    // heartbeat later, by which time the whole batch has landed in one place.
    // Measured: six apps, three nodes, 6/0/0.
    pending: &BTreeMap<String, usize>,
) -> Result<Vec<(String, u32)>, String> {
    let eligible: Vec<&NodeInventory> = observed.iter().filter(|n| fits(c, n)).collect();

    match c.placement.mode {
        Mode::Pinned => {
            let missing: Vec<&str> = c
                .placement
                .nodes
                .iter()
                .filter(|want| !eligible.iter().any(|n| n.node == **want))
                .map(|s| s.as_str())
                .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "pinned to {missing:?}, which {} not available or do not meet the component's constraints",
                    if missing.len() == 1 { "is" } else { "are" }
                ));
            }
            Ok(c.placement.nodes.iter().map(|n| (n.clone(), 1)).collect())
        }
        _ if eligible.is_empty() => Err(format!(
            "no node satisfies constraints {:?} and host interfaces {:?}",
            c.placement.constraints, c.host_needs
        )),
        Mode::Daemon => Ok(eligible.iter().map(|n| (n.node.clone(), 1)).collect()),
        Mode::Spread => {
            if replicas == 0 {
                return Ok(Vec::new());
            }
            // Three keys, in this order, and each is load-bearing:
            //
            //  1. replicas of THIS component already here, DESCENDING — stability.
            //     A node joining must not shuffle every existing replica onto it.
            //  2. total instances on the node, ASCENDING — balance ACROSS apps.
            //     Without this, every new app scores 0 on key 1 and the tie-break
            //     is the node name, so N different apps all land on the
            //     alphabetically first node. Measured: six apps, five nodes, one
            //     machine holding all six. That is worse than pinning tenants to
            //     machines, which is the thing this platform exists not to do.
            //  3. the name — determinism, so the plan is a pure function.
            let mut ranked: Vec<(&NodeInventory, u32, usize)> = eligible
                .iter()
                .map(|n| {
                    let observed_load: usize = n.instances.iter().map(|i| i.count as usize).sum();
                    (*n, running_on(c, n), observed_load + pending.get(&n.node).copied().unwrap_or(0))
                })
                .collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)).then(a.0.node.cmp(&b.0.node)));

            let n = ranked.len() as u32;
            let (base, rem) = (replicas / n, replicas % n);
            Ok(ranked
                .iter()
                .enumerate()
                .map(|(i, (node, _, _))| {
                    (node.node.clone(), base + if (i as u32) < rem { 1 } else { 0 })
                })
                .filter(|(_, count)| *count > 0)
                .collect())
        }
    }
}

fn fits(c: &Component, n: &NodeInventory) -> bool {
    c.placement.constraints.iter().all(|(k, v)| n.labels.get(k) == Some(v))
        && c.host_needs.iter().all(|need| n.host_ifaces.contains(need))
}

fn running_on(c: &Component, n: &NodeInventory) -> u32 {
    n.instances
        .iter()
        .filter(|i| i.component == c.id && i.digest == c.digest)
        .map(|i| i.count)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, labels: &[(&str, &str)], ifaces: &[&str]) -> NodeInventory {
        NodeInventory {
            node: name.into(),
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            host_ifaces: ifaces.iter().map(|s| s.to_string()).collect(),
            kv_shared: false,
            address: String::new(),
            instances: Vec::new(),
        }
    }

    /// A node whose store every replica can see — `--kv nats`.
    fn shared(name: &str) -> NodeInventory {
        NodeInventory { kv_shared: true, ..node(name, &[], &[]) }
    }

    /// A component that keeps state, so the split-brain check applies to it.
    fn stateful(id: &str, digest: &str, replicas: u32) -> Component {
        Component {
            host_needs: vec!["wasi:keyvalue/store".into()],
            ..comp(id, digest, replicas)
        }
    }

    fn running(inv: &mut NodeInventory, component: &str, digest: &str, count: u32) {
        inv.instances.push(RunningInstance {
            tenant: "alice".into(),
            app: "mesh".into(),
            component: component.into(),
            digest: digest.into(),
            count,
            ingress_host: None,
        });
    }

    fn comp(id: &str, digest: &str, replicas: u32) -> Component {
        Component {
            id: id.into(),
            digest: digest.into(),
            replicas,
            scale: None,
            placement: Placement::default(),
            host_needs: Vec::new(),
            config: BTreeMap::new(),
            secrets: Vec::new(),
            egress: Vec::new(),
        }
    }

    fn app(components: Vec<Component>, links: Vec<Link>, strategy: Strategy) -> Manifest {
        let root = components[0].id.clone();
        Manifest {
            app: "mesh".into(),
            tenant: "alice".into(),
            strategy,
            components,
            links,
            ingress: Some(Ingress { host: "mesh.example.com".into(), component: root }),
        }
    }

    /// Run to convergence, so a test can assert on the settled world rather than on
    /// one pass. Applies commands to the inventory the way a host would.
    fn converge(desired: &[Manifest], observed: &mut Vec<NodeInventory>, passes: u32) -> Outcome {
        let cfg = Cfg::default();
        let mut hyst = Hysteresis::default();
        let mut last = Outcome::default();
        for _ in 0..passes {
            last = plan(desired, observed, &Load::new(), &mut hyst, &cfg);
            for cmd in &last.commands {
                apply(observed, cmd);
            }
        }
        last
    }

    fn apply(observed: &mut [NodeInventory], cmd: &Command) {
        match cmd {
            Command::Start { node, tenant, app, component, digest, count, .. } => {
                let inv = observed.iter_mut().find(|n| n.node == *node).expect("node");
                match inv.instances.iter_mut().find(|i| i.component == *component && i.digest == *digest)
                {
                    // Absolute, not additive — see the note in `plan`.
                    Some(i) => i.count = *count,
                    None => inv.instances.push(RunningInstance {
                        tenant: tenant.clone(),
                        app: app.clone(),
                        component: component.clone(),
                        digest: digest.clone(),
                        count: *count,
                        ingress_host: None,
                    }),
                }
            }
            Command::Stop { node, component, digest, .. } => {
                let inv = observed.iter_mut().find(|n| n.node == *node).expect("node");
                inv.instances.retain(|i| !(i.component == *component && i.digest == *digest));
            }
        }
    }

    fn counts(observed: &[NodeInventory], component: &str) -> Vec<(String, u32)> {
        observed
            .iter()
            .map(|n| (n.node.clone(), n.instances.iter().filter(|i| i.component == component).map(|i| i.count).sum()))
            .filter(|(_, c)| *c > 0)
            .collect()
    }

    #[test]
    fn a_deficit_is_filled_on_the_first_pass() {
        // Under-replicated is the bad direction: it must never wait for hysteresis.
        let m = app(vec![comp("api", "sha256:a", 2)], vec![], Strategy::Linked);
        let obs = vec![node("box-a", &[], &[]), node("box-b", &[], &[])];
        let out = plan(&[m], &obs, &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        assert_eq!(out.commands.len(), 2, "{:?}", out.commands);
        assert!(out.commands.iter().all(|c| matches!(c, Command::Start { count: 1, .. })));
    }

    #[test]
    fn a_surplus_waits_for_two_consecutive_passes() {
        let m = app(vec![comp("api", "sha256:a", 1)], vec![], Strategy::Linked);
        let mut a = node("box-a", &[], &[]);
        running(&mut a, "api", "sha256:a", 3);
        let obs = vec![a];
        let cfg = Cfg::default();
        let mut hyst = Hysteresis::default();

        let first = plan(&[m.clone()], &obs, &Load::new(), &mut hyst, &cfg);
        assert!(first.commands.is_empty(), "must not stop on the first sighting");
        // Absolute: "hold 1", not "drop 2". Re-sending it is a no-op.
        let second = plan(&[m], &obs, &Load::new(), &mut hyst, &cfg);
        assert!(matches!(&second.commands[..], [Command::Start { count: 1, node, .. }] if node == "box-a"),
            "{:?}", second.commands);
    }

    #[test]
    fn a_surplus_that_goes_away_resets_the_counter() {
        // The flap case: one pass of surplus, then not, then surplus again must not
        // add up to a stop.
        let m = app(vec![comp("api", "sha256:a", 1)], vec![], Strategy::Linked);
        let mut over = node("box-a", &[], &[]);
        running(&mut over, "api", "sha256:a", 2);
        let mut exact = node("box-a", &[], &[]);
        running(&mut exact, "api", "sha256:a", 1);
        let cfg = Cfg::default();
        let mut hyst = Hysteresis::default();

        assert!(plan(&[m.clone()], &[over.clone()], &Load::new(), &mut hyst, &cfg).commands.is_empty());
        assert!(plan(&[m.clone()], &[exact], &Load::new(), &mut hyst, &cfg).commands.is_empty());
        assert!(
            plan(&[m], &[over], &Load::new(), &mut hyst, &cfg).commands.is_empty(),
            "the counter must have restarted, not carried over"
        );
    }

    #[test]
    fn a_vanished_node_is_replaced_without_any_delete_handling() {
        // The property that removes ADR-0016's whole reaping apparatus: a node that
        // stops writing its inventory key simply stops appearing.
        let m = app(vec![comp("api", "sha256:a", 2)], vec![], Strategy::Linked);
        let mut obs = vec![node("box-a", &[], &[]), node("box-b", &[], &[])];
        converge(&[m.clone()], &mut obs, 2);
        assert_eq!(counts(&obs, "api"), vec![("box-a".into(), 1), ("box-b".into(), 1)]);

        obs.retain(|n| n.node != "box-b"); // box-b is gone
        converge(&[m], &mut obs, 2);
        assert_eq!(counts(&obs, "api"), vec![("box-a".into(), 2)], "both replicas land on the survivor");
    }

    #[test]
    fn a_joining_node_does_not_shuffle_existing_replicas() {
        // Spread has to be stable or every node join is a fleet-wide restart.
        let m = app(vec![comp("api", "sha256:a", 2)], vec![], Strategy::Linked);
        let mut obs = vec![node("box-a", &[], &[])];
        converge(&[m.clone()], &mut obs, 2);
        assert_eq!(counts(&obs, "api"), vec![("box-a".into(), 2)]);

        obs.push(node("box-b", &[], &[]));
        let out = converge(&[m], &mut obs, 3);
        assert!(out.commands.is_empty(), "settled: {:?}", out.commands);
        // One moves, one stays. Not both moving, and not a stop-then-start of each.
        assert_eq!(counts(&obs, "api"), vec![("box-a".into(), 1), ("box-b".into(), 1)]);
    }

    #[test]
    fn constraints_and_host_needs_both_gate_placement() {
        let mut c = comp("api", "sha256:a", 1);
        c.placement.constraints.insert("region".into(), "eu-central".into());
        c.host_needs.push("wasi:keyvalue/store@0.2.0-draft".into());
        let m = app(vec![c], vec![], Strategy::Linked);

        let obs = vec![
            node("wrong-region", &[("region", "us-east")], &["wasi:keyvalue/store@0.2.0-draft"]),
            node("no-kv", &[("region", "eu-central")], &[]),
            node("good", &[("region", "eu-central")], &["wasi:keyvalue/store@0.2.0-draft"]),
        ];
        let out = plan(&[m], &obs, &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        assert_eq!(out.commands.len(), 1);
        assert_eq!(out.commands[0].node(), "good");
    }

    #[test]
    fn nothing_eligible_is_reported_not_silently_dropped() {
        let mut c = comp("api", "sha256:a", 1);
        c.placement.constraints.insert("region".into(), "antarctica".into());
        let m = app(vec![c], vec![], Strategy::Linked);
        let out = plan(&[m], &[node("box-a", &[], &[])], &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        assert!(out.commands.is_empty());
        assert_eq!(out.unschedulable.len(), 1);
        assert!(out.unschedulable[0].reason.contains("antarctica"), "{:?}", out.unschedulable);
    }

    #[test]
    fn a_pinned_node_that_is_gone_is_unschedulable_not_relocated() {
        // Pinned means pinned. Quietly placing it elsewhere would defeat whatever
        // the pin was for — a GPU, a jurisdiction.
        let mut c = comp("api", "sha256:a", 1);
        c.placement.mode = Mode::Pinned;
        c.placement.nodes = vec!["box-gpu".into()];
        let m = app(vec![c], vec![], Strategy::Linked);
        let out = plan(&[m], &[node("box-a", &[], &[])], &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        assert!(out.commands.is_empty());
        assert!(out.unschedulable[0].reason.contains("box-gpu"), "{:?}", out.unschedulable);
    }

    #[test]
    fn daemon_puts_one_on_every_eligible_node_and_ignores_replicas() {
        let mut c = comp("api", "sha256:a", 7);
        c.placement.mode = Mode::Daemon;
        let m = app(vec![c], vec![], Strategy::Linked);
        let mut obs = vec![node("box-a", &[], &[]), node("box-b", &[], &[]), node("box-c", &[], &[])];
        converge(&[m], &mut obs, 2);
        assert_eq!(
            counts(&obs, "api"),
            vec![("box-a".into(), 1), ("box-b".into(), 1), ("box-c".into(), 1)]
        );
    }

    #[test]
    fn a_linked_graph_co_locates_and_every_link_resolves_locally() {
        // The property that lets the host bind a link to a direct in-process call.
        let m = app(
            vec![comp("api", "sha256:a", 2), comp("store", "sha256:b", 1)],
            vec![Link {
                plug: "store".into(),
                socket: "api".into(),
                iface: "records:store/store@0.1.0".into(),
            }],
            Strategy::Linked,
        );
        let mut obs = vec![node("box-a", &[], &[]), node("box-b", &[], &[])];
        converge(&[m], &mut obs, 2);

        for n in &obs {
            let ids: BTreeSet<&str> = n.instances.iter().map(|i| i.component.as_str()).collect();
            assert!(ids.contains("api") && ids.contains("store"), "{}: {ids:?}", n.node);
        }
    }

    #[test]
    fn a_start_carries_the_link_table_the_host_binds_against() {
        let m = app(
            vec![comp("api", "sha256:a", 1), comp("store", "sha256:b", 1)],
            vec![Link {
                plug: "store".into(),
                socket: "api".into(),
                iface: "records:store/store@0.1.0".into(),
            }],
            Strategy::Linked,
        );
        let out = plan(&[m], &[node("box-a", &[], &[])], &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        let api = out
            .commands
            .iter()
            .find(|c| matches!(c, Command::Start { component, .. } if component == "api"))
            .expect("api start");
        let Command::Start { links, .. } = api else { unreachable!() };
        assert_eq!(links["records:store/store@0.1.0"], "alice/mesh/store");

        // The plug itself imports nothing from this graph.
        let store = out
            .commands
            .iter()
            .find(|c| matches!(c, Command::Start { component, .. } if component == "store"))
            .expect("store start");
        let Command::Start { links, .. } = store else { unreachable!() };
        assert!(links.is_empty());
    }

    #[test]
    fn fused_starts_only_the_root() {
        // `wac plug` erased the parts at build time; the manifest keeps them as the
        // build recipe, but nothing else runs.
        let m = app(
            vec![comp("api", "sha256:fused", 1), comp("store", "sha256:b", 1)],
            vec![Link {
                plug: "store".into(),
                socket: "api".into(),
                iface: "records:store/store@0.1.0".into(),
            }],
            Strategy::Fused,
        );
        let out = plan(&[m], &[node("box-a", &[], &[])], &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        assert_eq!(out.commands.len(), 1);
        assert!(matches!(&out.commands[0], Command::Start { component, .. } if component == "api"));
    }

    #[test]
    fn a_new_digest_starts_before_the_old_one_stops() {
        let mut a = node("box-a", &[], &[]);
        running(&mut a, "api", "sha256:old", 1);
        let m = app(vec![comp("api", "sha256:new", 1)], vec![], Strategy::Linked);
        let cfg = Cfg::default();
        let mut hyst = Hysteresis::default();

        let first = plan(&[m.clone()], &[a.clone()], &Load::new(), &mut hyst, &cfg);
        assert_eq!(first.commands.len(), 1, "the new one comes up alone first");
        assert!(matches!(&first.commands[0], Command::Start { digest, .. } if digest == "sha256:new"));

        let second = plan(&[m], &[a], &Load::new(), &mut hyst, &cfg);
        assert!(second.commands.iter().any(
            |c| matches!(c, Command::Stop { digest, .. } if digest == "sha256:old")
        ));
        // Ordering holds within a pass too.
        let kinds: Vec<bool> =
            second.commands.iter().map(|c| matches!(c, Command::Start { .. })).collect();
        let first_stop = kinds.iter().position(|s| !s).unwrap_or(kinds.len());
        assert!(kinds[..first_stop].iter().all(|s| *s), "starts must precede stops");
    }

    #[test]
    fn an_app_that_leaves_the_desired_set_is_stopped() {
        let mut a = node("box-a", &[], &[]);
        running(&mut a, "api", "sha256:a", 1);
        let cfg = Cfg::default();
        let mut hyst = Hysteresis::default();
        plan(&[], &[a.clone()], &Load::new(), &mut hyst, &cfg);
        let out = plan(&[], &[a], &Load::new(), &mut hyst, &cfg);
        assert!(matches!(&out.commands[0], Command::Stop { app, .. } if app == "mesh"));
    }

    #[test]
    fn the_command_cap_defers_rather_than_drops() {
        // A silent truncation reads as "converged" when it is not, so the count is
        // reported and the next pass re-derives the rest.
        let m = app(vec![comp("api", "sha256:a", 50)], vec![], Strategy::Linked);
        let obs: Vec<NodeInventory> =
            (0..50).map(|i| node(&format!("box-{i:02}"), &[], &[])).collect();
        let cfg = Cfg { max_commands: 20, ..Cfg::default() };
        let out = plan(&[m], &obs, &Load::new(), &mut Hysteresis::default(), &cfg);
        assert_eq!(out.commands.len(), 20);
        assert_eq!(out.deferred, 30);
    }


    /// THE test. This is the bug that shipped: two replicas, node-local stores,
    /// each getting its own store under the same bucket name. Nothing errored — the
    /// rate limiter just stopped rate-limiting, and the failover moved placement
    /// without the data.
    #[test]
    fn a_stateful_app_is_refused_rather_than_split_across_local_stores() {
        let m = app(vec![stateful("api", "sha256:a", 2)], vec![], Strategy::Linked);
        let obs = vec![node("box-a", &[], &["wasi:keyvalue/store"]),
                       node("box-b", &[], &["wasi:keyvalue/store"])];
        let out = plan(&[m], &obs, &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        assert!(out.commands.is_empty(), "nothing may be placed: {:?}", out.commands);
        let reason = &out.unschedulable[0].reason;
        assert!(reason.contains("diverge in silence"), "{reason}");
        // The reason has to name the offending nodes, or the operator cannot act.
        assert!(reason.contains("box-a") && reason.contains("box-b"), "{reason}");
    }

    #[test]
    fn the_same_app_places_fine_on_nodes_with_a_shared_store() {
        let m = app(vec![stateful("api", "sha256:a", 2)], vec![], Strategy::Linked);
        let mut a = shared("box-a");
        let mut b = shared("box-b");
        a.host_ifaces = vec!["wasi:keyvalue/store".into()];
        b.host_ifaces = vec!["wasi:keyvalue/store".into()];
        let out = plan(&[m], &[a, b], &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        assert_eq!(out.commands.len(), 2, "{:?}", out.unschedulable);
        assert!(out.unschedulable.is_empty());
    }

    #[test]
    fn one_replica_on_a_local_store_is_fine() {
        // The single-node self-hosting lane, which is where sqlite came from and
        // where it is exactly right. Refusing this would break that lane.
        let m = app(vec![stateful("api", "sha256:a", 1)], vec![], Strategy::Linked);
        let obs = vec![node("box-a", &[], &["wasi:keyvalue/store"]),
                       node("box-b", &[], &["wasi:keyvalue/store"])];
        let out = plan(&[m], &obs, &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        assert_eq!(out.commands.len(), 1);
        assert!(out.unschedulable.is_empty());
    }

    #[test]
    fn a_stateless_app_spreads_freely_over_local_stores() {
        // No keyvalue import, nothing to diverge. The check must not become a
        // blanket ban on spreading.
        let m = app(vec![comp("api", "sha256:a", 2)], vec![], Strategy::Linked);
        let obs = vec![node("box-a", &[], &[]), node("box-b", &[], &[])];
        let out = plan(&[m], &obs, &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        assert_eq!(out.commands.len(), 2);
        assert!(out.unschedulable.is_empty());
    }

    #[test]
    fn a_plug_holding_state_counts_even_when_the_root_does_not() {
        // The graph is co-located, so a stateful PLUG lands on every node the root
        // does. Looking only at the root would miss it.
        let m = app(
            vec![comp("api", "sha256:a", 2), stateful("store", "sha256:b", 1)],
            vec![Link { plug: "store".into(), socket: "api".into(), iface: "records:store/store@0.1.0".into() }],
            Strategy::Linked,
        );
        let obs = vec![node("box-a", &[], &["wasi:keyvalue/store"]),
                       node("box-b", &[], &["wasi:keyvalue/store"])];
        let out = plan(&[m], &obs, &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        assert!(out.commands.is_empty(), "{:?}", out.commands);
        assert!(out.unschedulable[0].reason.contains("diverge"));
    }

    #[test]
    fn an_unreported_kv_shared_reads_as_node_local() {
        // Fail closed. A node predating this field, or one whose inventory we only
        // partly parsed, must not be treated as safe to spread onto.
        let inv: NodeInventory = serde_json::from_str(
            r#"{"node":"old","host_ifaces":["wasi:keyvalue/store"],"instances":[]}"#,
        )
        .expect("parses");
        assert!(!inv.kv_shared, "an absent kv_shared must read as node-local");
    }

    #[test]
    fn a_plug_with_its_own_placement_lands_on_a_different_node() {
        // The change that makes a graph SPAN. Without it every component of a
        // linked app is pinned to the root's nodes and a cross-node call can never
        // happen, however well the transport works.
        let mut plug = comp("store", "sha256:b", 1);
        plug.placement.constraints.insert("role".into(), "data".into());
        // The root is pinned to the web tier too, so the test asserts placement
        // rather than tie-breaking order.
        let mut root = comp("api", "sha256:a", 1);
        root.placement.constraints.insert("role".into(), "web".into());
        let m = app(
            vec![root, plug],
            vec![Link { plug: "store".into(), socket: "api".into(), iface: "records:store/store@0.1.0".into() }],
            Strategy::Linked,
        );
        let obs = vec![node("edge", &[("role", "web")], &[]), node("data-1", &[("role", "data")], &[])];
        let out = plan(&[m], &obs, &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        let where_ = |c: &str| {
            out.commands
                .iter()
                .find(|x| matches!(x, Command::Start { component, .. } if component == c))
                .map(|x| x.node().to_string())
        };
        assert_eq!(where_("api").as_deref(), Some("edge"));
        assert_eq!(where_("store").as_deref(), Some("data-1"), "the plug follows its own placement");
        assert_ne!(where_("api"), where_("store"), "the graph must actually span");
    }

    #[test]
    fn a_plug_without_its_own_placement_still_rides_along() {
        // Co-location stays the default: it is faster (ADR-0019's 1.2ms) and the
        // spanning case should be opted into, not fallen into.
        let m = app(
            vec![comp("api", "sha256:a", 1), comp("store", "sha256:b", 1)],
            vec![Link { plug: "store".into(), socket: "api".into(), iface: "records:store/store@0.1.0".into() }],
            Strategy::Linked,
        );
        let obs = vec![node("n1", &[], &[]), node("n2", &[], &[])];
        let out = plan(&[m], &obs, &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        let nodes: std::collections::BTreeSet<&str> =
            out.commands.iter().map(|c| c.node()).collect();
        assert_eq!(nodes.len(), 1, "both parts on one node: {nodes:?}");
    }

    #[test]
    fn a_plug_that_cannot_be_placed_is_reported_against_its_own_name() {
        // "app unschedulable" without saying which component is a bad error when a
        // graph has ten of them.
        let mut plug = comp("store", "sha256:b", 1);
        plug.placement.constraints.insert("role".into(), "gpu".into());
        let m = app(
            vec![comp("api", "sha256:a", 1), plug],
            vec![],
            Strategy::Linked,
        );
        let out = plan(&[m], &[node("n1", &[], &[])], &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        assert!(out.unschedulable[0].reason.starts_with("`store`:"), "{:?}", out.unschedulable);
    }

    #[test]
    fn different_apps_spread_across_nodes_instead_of_piling_onto_one() {
        // THE bug this ranking exists for. Every app is new, so every node scores 0
        // on "replicas of this component" — and without a load key the tie-break is
        // the node name, putting all six on the alphabetically first node.
        let mut obs = vec![node("n1", &[], &[]), node("n2", &[], &[]), node("n3", &[], &[])];
        let apps: Vec<Manifest> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(|n| {
                let mut m = app(vec![comp(n, "sha256:x", 1)], vec![], Strategy::Linked);
                m.app = n.to_string();
                m
            })
            .collect();
        converge(&apps, &mut obs, 3);
        let per_node: Vec<usize> = obs.iter().map(|n| n.instances.len()).collect();
        assert_eq!(per_node.iter().sum::<usize>(), 6);
        assert!(
            per_node.iter().all(|c| *c == 2),
            "six apps over three nodes should be 2/2/2, got {per_node:?}"
        );
    }

    #[test]
    fn balancing_across_apps_does_not_break_stability_within_one() {
        // Key 1 still wins: an existing replica must not move just because its node
        // is now carrying more total load than a neighbour.
        let m = app(vec![comp("api", "sha256:a", 1)], vec![], Strategy::Linked);
        let mut busy = node("busy", &[], &[]);
        running(&mut busy, "api", "sha256:a", 1);
        running(&mut busy, "other", "sha256:z", 5);
        let idle = node("idle", &[], &[]);
        let out = plan(&[m], &[busy, idle], &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        assert!(out.commands.is_empty(), "it must stay put: {:?}", out.commands);
    }

    #[test]
    fn a_settled_world_emits_nothing() {
        // The property everything else depends on: convergence is a fixed point, so
        // a healthy fleet is silent rather than churning.
        let m = app(
            vec![comp("api", "sha256:a", 3), comp("store", "sha256:b", 1)],
            vec![Link {
                plug: "store".into(),
                socket: "api".into(),
                iface: "records:store/store@0.1.0".into(),
            }],
            Strategy::Linked,
        );
        let mut obs = vec![node("box-a", &[], &[]), node("box-b", &[], &[])];
        converge(&[m.clone()], &mut obs, 3);
        let out = plan(&[m], &obs, &Load::new(), &mut Hysteresis::default(), &Cfg::default());
        assert!(out.commands.is_empty(), "not settled: {:?}", out.commands);
        assert!(out.unschedulable.is_empty());
    }

    fn scaled(min: u32, max: u32, target: u32) -> Component {
        let mut c = comp("gate", "sha256:aa", 1);
        c.scale = Some(Scale { min, max, target });
        c
    }

    fn app_with(c: Component, host: &str) -> Manifest {
        let mut m = app(vec![c], vec![], Strategy::Fused);
        m.ingress = Some(Ingress { host: host.into(), component: "gate".into() });
        m
    }

    #[test]
    fn replicas_track_concurrency_between_min_and_max() {
        let m = app_with(scaled(1, 10, 10), "shop.eve.test");
        let mut load = Load::new();
        for (inflight, want) in [(0, 1), (1, 1), (10, 1), (11, 2), (25, 3), (100, 10), (500, 10)] {
            load.insert("shop.eve.test".into(), inflight);
            assert_eq!(
                desired_replicas(&m.components[0], &m, &load),
                want,
                "{inflight} in flight at target 10 should want {want}"
            );
        }
    }

    #[test]
    fn a_missing_sample_holds_the_count_instead_of_scaling_to_min() {
        // THE failure mode. The ingress restarting, or the very first pass before a
        // sample exists, must not read as "no traffic" — that would scale a busy app
        // to `min` (possibly zero) precisely when nobody is watching it.
        let mut c = scaled(0, 10, 10);
        c.replicas = 4;
        let m = app_with(c, "shop.eve.test");
        assert_eq!(
            desired_replicas(&m.components[0], &m, &Load::new()),
            4,
            "no sample must hold the current count, not collapse to min"
        );
    }

    #[test]
    fn an_app_with_no_scale_block_is_untouched_by_load() {
        // Every existing manifest is this case. Load must be inert for them.
        let m = app_with(comp("gate", "sha256:aa", 3), "shop.eve.test");
        let load = Load::from([("shop.eve.test".to_string(), 900)]);
        assert_eq!(desired_replicas(&m.components[0], &m, &load), 3);
    }

    #[test]
    fn scale_to_zero_is_reachable_and_a_request_brings_it_back() {
        let m = app_with(scaled(0, 5, 10), "shop.eve.test");
        let idle = Load::from([("shop.eve.test".to_string(), 0)]);
        assert_eq!(desired_replicas(&m.components[0], &m, &idle), 0, "idle scales to zero");
        let one = Load::from([("shop.eve.test".to_string(), 1)]);
        assert_eq!(desired_replicas(&m.components[0], &m, &one), 1, "one request brings it back");
    }

    #[test]
    fn a_nonsense_scale_block_cannot_produce_a_nonsense_count() {
        // max below min, and a target of zero, are both things a hand-written
        // manifest will contain eventually. Neither may panic or divide by zero.
        let m = app_with(scaled(3, 1, 0), "shop.eve.test");
        let load = Load::from([("shop.eve.test".to_string(), 7)]);
        let n = desired_replicas(&m.components[0], &m, &load);
        assert!(n >= 3, "min must still hold when max is below it, got {n}");
    }

    #[test]
    fn scaling_up_actually_places_the_new_replicas() {
        // desired_replicas is arithmetic; this checks the number reaches placement.
        let m = app_with(scaled(1, 4, 10), "shop.eve.test");
        let nodes = vec![node("n1", &[], &[]), node("n2", &[], &[])];
        let load = Load::from([("shop.eve.test".to_string(), 31)]);
        let mut h = Hysteresis::default();
        let out = plan(&[m], &nodes, &load, &mut h, &Cfg::default());
        let total: u32 = out
            .commands
            .iter()
            .filter_map(|c| match c {
                Command::Start { count, .. } => Some(*count),
                _ => None,
            })
            .sum();
        assert_eq!(total, 4, "31 in flight at target 10 is 4 replicas: {:?}", out.commands);
    }

}
